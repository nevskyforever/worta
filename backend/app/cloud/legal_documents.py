"""Versioned server authority for WORTA account-level legal acknowledgements."""
from __future__ import annotations

import hashlib
from dataclasses import dataclass
from typing import Literal, Sequence
from uuid import UUID

from sqlalchemy import select
from sqlalchemy.dialects.postgresql import insert
from sqlalchemy.orm import Session

from .models import UserDocumentAcceptance


DocumentType = Literal[
    'user_agreement', 'privacy_policy', 'personal_data_consent', 'beta_terms',
]
AcceptanceMethod = Literal['invitation', 'authenticated_confirmation']

SUPPORTED_DOCUMENT_TYPES: frozenset[str] = frozenset({
    'user_agreement', 'privacy_policy', 'personal_data_consent', 'beta_terms',
})
SUPPORTED_LANGUAGES: frozenset[str] = frozenset({'ru', 'en', 'es', 'de', 'fr', 'pt_BR'})


@dataclass(frozen=True, slots=True)
class LegalDocumentVersion:
    document_id: str
    document_type: DocumentType
    revision: str
    language: str
    title: str
    content: str
    mandatory: bool
    requires_explicit_acceptance: bool
    approved: bool

    @property
    def content_checksum(self) -> str:
        return hashlib.sha256(self.content.encode('utf-8')).hexdigest()


@dataclass(frozen=True, slots=True)
class DocumentConfirmation:
    document_id: str
    revision: str
    content_checksum: str
    language: str


class LegalDocumentError(Exception):
    def __init__(self, code: str, status_code: int = 409) -> None:
        self.code = code
        self.status_code = status_code
        super().__init__(code)


BETA_WARNING_RU = """Вы участвуете в закрытом тестировании worta.

Приложение находится в разработке. Возможны ошибки, временная недоступность функций и сервиса, проблемы с синхронизацией, повреждение или потеря данных.

Не рекомендуется хранить единственные экземпляры важных материалов в тестовой версии. Регулярно создавайте резервные копии.

Перед продолжением ознакомьтесь с условиями тестирования и подтвердите их принятие"""


# The owner supplied and approved this exact private-beta warning in
# LEGAL.ACCEPTANCE.01. The other supported document types intentionally have no
# approved production text yet; the obsolete nfprogress agreement is not reused.
DEFAULT_LEGAL_DOCUMENTS: tuple[LegalDocumentVersion, ...] = (
    LegalDocumentVersion(
        document_id='wotra.user_agreement', document_type='user_agreement',
        revision='draft-unapproved-1', language='ru', title='Пользовательское соглашение WORTA',
        content='DRAFT — текст не утверждён владельцем и не доступен для принятия.',
        mandatory=False, requires_explicit_acceptance=True, approved=False,
    ),
    LegalDocumentVersion(
        document_id='wotra.privacy_policy', document_type='privacy_policy',
        revision='draft-unapproved-1', language='ru', title='Политика конфиденциальности WORTA',
        content='DRAFT — текст не утверждён владельцем и не доступен пользователям.',
        mandatory=False, requires_explicit_acceptance=False, approved=False,
    ),
    LegalDocumentVersion(
        document_id='wotra.personal_data_consent', document_type='personal_data_consent',
        revision='draft-unapproved-1', language='ru', title='Согласие на обработку персональных данных',
        content='DRAFT — применимость и текст не утверждены владельцем.',
        mandatory=False, requires_explicit_acceptance=True, approved=False,
    ),
    LegalDocumentVersion(
        document_id='wotra.beta_terms',
        document_type='beta_terms',
        revision='private-beta-warning-brand-2',
        language='ru',
        title='Закрытое тестирование worta',
        content=BETA_WARNING_RU,
        mandatory=True,
        requires_explicit_acceptance=True,
        approved=True,
    ),
)


class LegalDocumentRegistry:
    """Immutable exact-text registry; a changed translation is a new revision."""

    def __init__(self, versions: Sequence[LegalDocumentVersion] = DEFAULT_LEGAL_DOCUMENTS) -> None:
        self._versions = tuple(versions)
        keys: set[tuple[str, str, str]] = set()
        for version in self._versions:
            if version.document_type not in SUPPORTED_DOCUMENT_TYPES:
                raise ValueError('Unsupported legal document type.')
            if version.language not in SUPPORTED_LANGUAGES:
                raise ValueError('Unsupported legal document language.')
            key = (version.document_id, version.revision, version.language)
            if key in keys:
                raise ValueError('Duplicate legal document version.')
            keys.add(key)

    def current(self, preferred_language: str) -> tuple[LegalDocumentVersion, ...]:
        language = preferred_language if preferred_language in SUPPORTED_LANGUAGES else 'ru'
        approved = [version for version in self._versions if version.approved]
        document_ids = {version.document_id for version in approved}
        selected: list[LegalDocumentVersion] = []
        for document_id in sorted(document_ids):
            candidates = [version for version in approved if version.document_id == document_id]
            latest_revision = candidates[-1].revision
            latest = [version for version in candidates if version.revision == latest_revision]
            localized = [version for version in latest if version.language == language]
            fallback = [version for version in latest if version.language == 'ru']
            usable = localized or fallback
            if not usable:
                continue
            # Registry order is explicit publication order; revision labels are
            # identifiers and are deliberately not interpreted as sortable numbers.
            selected.append(usable[-1])
        return tuple(selected)

    def current_localizations(self) -> tuple[LegalDocumentVersion, ...]:
        """Return every approved localization of each latest published revision."""
        approved = [version for version in self._versions if version.approved]
        selected: list[LegalDocumentVersion] = []
        for document_id in sorted({version.document_id for version in approved}):
            candidates = [version for version in approved if version.document_id == document_id]
            latest_revision = candidates[-1].revision
            selected.extend(version for version in candidates if version.revision == latest_revision)
        return tuple(selected)

    def verify(self, confirmations: Sequence[DocumentConfirmation], preferred_language: str) -> tuple[LegalDocumentVersion, ...]:
        current = self.current(preferred_language)
        required = tuple(version for version in current if version.mandatory)
        if not required:
            raise LegalDocumentError('legal_documents_unavailable', 503)
        supplied = {
            (item.document_id, item.revision, item.content_checksum, item.language)
            for item in confirmations
        }
        if len(supplied) != len(confirmations):
            raise LegalDocumentError('duplicate_document_confirmation', 422)
        expected = {
            (version.document_id, version.revision, version.content_checksum, version.language)
            for version in required
        }
        if supplied != expected:
            raise LegalDocumentError('legal_document_version_changed')
        return required


DEFAULT_LEGAL_DOCUMENT_REGISTRY = LegalDocumentRegistry()


class LegalAcceptanceService:
    def __init__(self, registry: LegalDocumentRegistry = DEFAULT_LEGAL_DOCUMENT_REGISTRY) -> None:
        self.registry = registry

    def status(self, session: Session, user_id: UUID, preferred_language: str) -> tuple[tuple[LegalDocumentVersion, ...], tuple[LegalDocumentVersion, ...]]:
        current = self.registry.current(preferred_language)
        if not any(version.mandatory for version in current):
            raise LegalDocumentError('legal_documents_unavailable', 503)
        current_localizations = self.registry.current_localizations()
        accepted = set(session.execute(select(
            UserDocumentAcceptance.document_id,
            UserDocumentAcceptance.revision,
            UserDocumentAcceptance.content_checksum,
            UserDocumentAcceptance.language,
        ).where(UserDocumentAcceptance.user_id == user_id)).all())
        missing = tuple(version for version in current if version.mandatory and not any((
            candidate.document_id, candidate.revision,
            candidate.content_checksum, candidate.language,
        ) in accepted for candidate in current_localizations if (
            candidate.document_id == version.document_id
            and candidate.revision == version.revision
        )))
        return current, missing

    def missing_for_gate(self, session: Session, user_id: UUID) -> tuple[LegalDocumentVersion, ...]:
        """Accept any exact current localization, never an older document revision."""
        approved = list(self.registry.current_localizations())
        if not any(version.mandatory for version in approved):
            raise LegalDocumentError('legal_documents_unavailable', 503)
        accepted = set(session.execute(select(
            UserDocumentAcceptance.document_id,
            UserDocumentAcceptance.revision,
            UserDocumentAcceptance.content_checksum,
            UserDocumentAcceptance.language,
        ).where(UserDocumentAcceptance.user_id == user_id)).all())
        missing: list[LegalDocumentVersion] = []
        for document_id in sorted({version.document_id for version in approved}):
            candidates = [version for version in approved if version.document_id == document_id]
            current = candidates
            if not any(version.mandatory for version in current):
                continue
            if not any((
                version.document_id, version.revision, version.content_checksum, version.language,
            ) in accepted for version in current):
                missing.append(next(
                    (version for version in current if version.language == 'ru'), current[0],
                ))
        return tuple(missing)

    def record(self, session: Session, *, user_id: UUID,
               confirmations: Sequence[DocumentConfirmation], preferred_language: str,
               method: AcceptanceMethod) -> tuple[LegalDocumentVersion, ...]:
        versions = self.registry.verify(confirmations, preferred_language)
        for version in versions:
            statement = insert(UserDocumentAcceptance).values(
                user_id=user_id,
                document_type=version.document_type,
                document_id=version.document_id,
                revision=version.revision,
                content_checksum=version.content_checksum,
                language=version.language,
                acceptance_method=method,
            ).on_conflict_do_nothing(constraint='uq_user_document_acceptance_exact')
            session.execute(statement)
        return versions
