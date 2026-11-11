from __future__ import annotations

from urllib.parse import urlencode

from fastapi import APIRouter, BackgroundTasks, Depends, HTTPException, Query, Request, Response, status
from sqlalchemy.exc import IntegrityError
from sqlalchemy.orm import Session

from ..dependencies import (AuthenticatedUser, get_authentication_service,
                            get_cloud_session, get_current_accepted_user,
                            get_current_user, get_email_sender)
from .email import EmailDeliveryError, EmailSender, OutgoingEmail
from .invitations import InvitationError, PrivateInvitationService
from .legal_documents import (DEFAULT_LEGAL_DOCUMENT_REGISTRY, DocumentConfirmation,
                              LegalAcceptanceService, LegalDocumentError)
from .schemas import (AccountLimitsResponse, AccountResponse, AuthenticatedLegalAcceptRequest,
                      CurrentUserCryptoResponse, InitialCryptoProvisioningRequest,
                      InvitationAcceptRequest, InvitationDocumentsRequest,
                      LegalDocumentResponse, LegalDocumentStatusResponse, LoginRequest,
                      PasswordKdfDto, PasswordResetConfirmRequest, PasswordWrappedAmkDto,
                      PasswordResetRequest, PublicVerificationRequest,
                      RecoveryWrappedAmkDto, RefreshRequest, RegistrationRequest, TokenResponse,
                      decode_canonical_base64url,
                      encode_canonical_base64url,
                      VerificationTokenRequest)
from .services import (AccountEmailService, AuthenticationError,
                       AuthenticationService, RecoveryTokenError,
                       LimitsService, LimitsUnavailableError, RegistrationService,
                       RegistrationUnavailableError)
from .tokens import TokenService


router = APIRouter(prefix='/api/v1', tags=['cloud authentication'])


def _legal_registry(request: Request):
    return getattr(request.app.state, 'legal_document_registry', DEFAULT_LEGAL_DOCUMENT_REGISTRY)


def _confirmations(payload) -> list[DocumentConfirmation]:
    return [DocumentConfirmation(
        document_id=item.document_id,
        revision=item.revision,
        content_checksum=item.content_checksum,
        language=item.language,
    ) for item in payload.documents]


def _legal_response(documents, missing) -> LegalDocumentStatusResponse:
    missing_keys = {
        (item.document_id, item.revision, item.content_checksum, item.language)
        for item in missing
    }
    return LegalDocumentStatusResponse(
        documents=[LegalDocumentResponse(
            document_id=item.document_id,
            document_type=item.document_type,
            revision=item.revision,
            content_checksum=item.content_checksum,
            language=item.language,
            title=item.title,
            content=item.content,
            mandatory=item.mandatory,
            requires_explicit_acceptance=item.requires_explicit_acceptance,
            accepted=(item.document_id, item.revision, item.content_checksum, item.language) not in missing_keys,
        ) for item in documents],
        missing_required_document_ids=[item.document_id for item in missing],
    )


def _authentication_failure() -> HTTPException:
    return HTTPException(status_code=status.HTTP_401_UNAUTHORIZED,
        detail={'code': 'invalid_credentials', 'message': 'Invalid credentials.'})


def _invalid_recovery_token() -> HTTPException:
    return HTTPException(status_code=400, detail={
        'code': 'invalid_or_expired_token', 'message': 'Invalid or expired token.',
    })


def _crypto_provisioning_conflict() -> HTTPException:
    return HTTPException(status_code=status.HTTP_409_CONFLICT, detail={
        'code': 'crypto_already_provisioned',
        'message': 'Account encryption is already provisioned.',
    })


def _same_crypto_record(record, payload: InitialCryptoProvisioningRequest) -> bool:
    password = payload.password
    recovery = payload.recovery
    return (
        record.password_crypto_version == password.crypto_version
        and record.password_wrapping_version == password.wrapping_version
        and record.kdf_version == password.kdf.kdf_version
        and record.kdf_algorithm == password.kdf.algorithm
        and record.kdf_salt == decode_canonical_base64url(password.kdf.salt, expected_length=16)
        and record.kdf_opslimit == password.kdf.opslimit
        and record.kdf_memlimit == password.kdf.memlimit
        and record.password_nonce == decode_canonical_base64url(password.nonce, expected_length=24)
        and record.password_wrapped_amk == decode_canonical_base64url(password.ciphertext, expected_length=48)
        and record.recovery_crypto_version == recovery.crypto_version
        and record.recovery_wrapping_version == recovery.wrapping_version
        and record.recovery_nonce == decode_canonical_base64url(recovery.nonce, expected_length=24)
        and record.recovery_wrapped_amk == decode_canonical_base64url(recovery.ciphertext, expected_length=48)
    )


def _email_service(request: Request) -> AccountEmailService:
    return AccountEmailService(TokenService(request.app.state.runtime_config.require_auth_secret()))


def _registration_service(request: Request) -> RegistrationService:
    return RegistrationService(TokenService(request.app.state.runtime_config.require_auth_secret()))


def _trusted_link(request: Request, path: str, token: str, *, fragment: bool = False) -> str | None:
    base = request.app.state.runtime_config.public_web_url
    if base is None:
        return None
    separator = "#" if fragment else "?"
    return f"{base.rstrip('/')}{path}{separator}{urlencode({'token': token})}"


def _send(sender: EmailSender, message: OutgoingEmail) -> None:
    try:
        sender.send(message)
    except EmailDeliveryError:
        # Public recovery endpoints retain their non-enumerating accepted response.
        # SmtpEmailSender already logged a sanitized failure.
        return


@router.get('/auth/registration')
def registration_policy(request: Request, session: Session = Depends(get_cloud_session)) -> dict[str, bool | str]:
    settings = _registration_service(request).public_policy(session)
    return {
        'mode': settings.mode,
        'registration_enabled': settings.mode != 'closed',
        'requires_approval': settings.mode == 'approval',
    }


@router.post('/auth/register', status_code=status.HTTP_202_ACCEPTED)
def register(payload: RegistrationRequest, background: BackgroundTasks, request: Request,
        session: Session = Depends(get_cloud_session), sender: EmailSender = Depends(get_email_sender)) -> dict[str, str]:
    config = request.app.state.runtime_config
    try:
        result = _registration_service(request).register(
            session, username=payload.username, email=payload.email, password=payload.password,
            email_delivery_available=config.environment != 'production' or config.email_delivery_configured,
        )
    except ValueError:
        raise HTTPException(status_code=422, detail={'code': 'invalid_password', 'message': 'Invalid password.'}) from None
    except RegistrationUnavailableError:
        raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail={
            'code': 'registration_unavailable', 'message': 'Registration is temporarily unavailable.',
        }) from None
    if result.code == 'registration_closed':
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail={
            'code': 'registration_closed', 'message': 'Registration is closed.',
        })
    if result.code == 'username_reserved':
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail={
            'code': 'username_reserved', 'message': 'This username is reserved.',
        })
    if result.verification_token and result.email:
        link = _trusted_link(request, '/verify-email', result.verification_token)
        if link:
            background.add_task(_send, sender, OutgoingEmail(result.email, 'Подтверждение email',
                f'Подтвердите email: {link}\nСсылка действует 24 часа. Если это были не вы, проигнорируйте письмо.'))
    return {'code': 'registration_request_accepted'}


@router.post('/auth/invitations/documents', response_model=LegalDocumentStatusResponse)
def invitation_documents(payload: InvitationDocumentsRequest, request: Request,
        response: Response, session: Session = Depends(get_cloud_session)) -> LegalDocumentStatusResponse:
    service = PrivateInvitationService(
        TokenService(request.app.state.runtime_config.require_auth_secret()),
        legal_registry=_legal_registry(request),
    )
    try:
        service.inspect(session, raw_token=payload.token)
        documents = service.documents(payload.language)
    except InvitationError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    except LegalDocumentError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    response.headers['Cache-Control'] = 'no-store'
    return _legal_response(documents, tuple(item for item in documents if item.mandatory))


@router.post('/auth/invitations/accept')
def accept_invitation(payload: InvitationAcceptRequest, request: Request,
        session: Session = Depends(get_cloud_session)) -> dict[str, str]:
    service = PrivateInvitationService(
        TokenService(request.app.state.runtime_config.require_auth_secret()),
        legal_registry=_legal_registry(request),
    )
    try:
        user = service.accept(
            session, raw_token=payload.token, password=payload.password,
            language=payload.language, documents=_confirmations(payload),
        )
    except ValueError:
        raise HTTPException(422, detail={'code': 'invalid_password'}) from None
    except InvitationError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    except LegalDocumentError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    return {'code': 'invitation_accepted', 'username': user.username}


@router.post('/auth/login', response_model=TokenResponse)
def login(payload: LoginRequest, session: Session = Depends(get_cloud_session),
          service: AuthenticationService = Depends(get_authentication_service)) -> TokenResponse:
    try:
        result = service.login(session, username=payload.username, password=payload.password)
    except AuthenticationError:
        raise _authentication_failure() from None
    return TokenResponse(access_token=result.access_token, refresh_token=result.refresh_token,
                         access_expires_in=result.access_expires_in)


@router.post('/auth/refresh', response_model=TokenResponse)
def refresh(payload: RefreshRequest, session: Session = Depends(get_cloud_session),
            service: AuthenticationService = Depends(get_authentication_service)) -> TokenResponse:
    try:
        result = service.refresh(session, payload.refresh_token)
    except AuthenticationError:
        raise _authentication_failure() from None
    return TokenResponse(access_token=result.access_token, refresh_token=result.refresh_token,
                         access_expires_in=result.access_expires_in)


@router.post('/auth/logout', status_code=status.HTTP_204_NO_CONTENT)
def logout(current: AuthenticatedUser = Depends(get_current_user),
           session: Session = Depends(get_cloud_session),
           service: AuthenticationService = Depends(get_authentication_service)) -> None:
    service.logout(session, current.session)


@router.get('/account/me', response_model=AccountResponse)
def account_me(current: AuthenticatedUser = Depends(get_current_user)) -> AccountResponse:
    user = current.user
    return AccountResponse(id=user.id, username=user.username, email=user.email,
        email_verified=user.email_verified, role=user.role, status=user.status, created_at=user.created_at)


@router.get('/legal/documents/status', response_model=LegalDocumentStatusResponse)
def legal_document_status(request: Request, response: Response,
        language: str = Query('ru', pattern='^(ru|en|es|de|fr|pt_BR)$'),
        current: AuthenticatedUser = Depends(get_current_user),
        session: Session = Depends(get_cloud_session)) -> LegalDocumentStatusResponse:
    try:
        documents, missing = LegalAcceptanceService(_legal_registry(request)).status(
            session, current.user.id, language,
        )
    except LegalDocumentError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    response.headers['Cache-Control'] = 'private, no-store'
    return _legal_response(documents, missing)


@router.post('/legal/documents/accept', response_model=LegalDocumentStatusResponse)
def accept_legal_documents(payload: AuthenticatedLegalAcceptRequest, request: Request,
        response: Response, current: AuthenticatedUser = Depends(get_current_user),
        session: Session = Depends(get_cloud_session)) -> LegalDocumentStatusResponse:
    service = LegalAcceptanceService(_legal_registry(request))
    try:
        service.record(
            session, user_id=current.user.id, confirmations=_confirmations(payload),
            preferred_language=payload.language, method='authenticated_confirmation',
        )
        session.commit()
    except LegalDocumentError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    documents, missing = service.status(session, current.user.id, payload.language)
    response.headers['Cache-Control'] = 'private, no-store'
    return _legal_response(documents, missing)


@router.get('/account/crypto', response_model=CurrentUserCryptoResponse)
def account_crypto(response: Response, current: AuthenticatedUser = Depends(get_current_accepted_user),
                   session: Session = Depends(get_cloud_session)) -> CurrentUserCryptoResponse:
    """Return only the current user's wrapped AMK record; never provision one implicitly."""
    from .models import UserCrypto

    response.headers['Cache-Control'] = 'private, no-store'
    record = session.get(UserCrypto, current.user.id)
    if record is None:
        return CurrentUserCryptoResponse(provisioned=False)
    password = PasswordWrappedAmkDto(
        crypto_version=record.password_crypto_version,
        wrapping_version=record.password_wrapping_version,
        kdf=PasswordKdfDto(
            kdf_version=record.kdf_version,
            algorithm=record.kdf_algorithm,
            salt=encode_canonical_base64url(record.kdf_salt),
            opslimit=record.kdf_opslimit,
            memlimit=record.kdf_memlimit,
        ),
        nonce=encode_canonical_base64url(record.password_nonce),
        ciphertext=encode_canonical_base64url(record.password_wrapped_amk),
    )
    recovery = None
    if record.recovery_wrapped_amk is not None:
        recovery = RecoveryWrappedAmkDto(
            crypto_version=record.recovery_crypto_version,
            wrapping_version=record.recovery_wrapping_version,
            nonce=encode_canonical_base64url(record.recovery_nonce),
            ciphertext=encode_canonical_base64url(record.recovery_wrapped_amk),
        )
    return CurrentUserCryptoResponse(
        provisioned=True,
        password=password,
        recovery=recovery,
    )


@router.post('/account/crypto', response_model=CurrentUserCryptoResponse,
             status_code=status.HTTP_201_CREATED)
def provision_account_crypto(
        payload: InitialCryptoProvisioningRequest,
        response: Response,
        current: AuthenticatedUser = Depends(get_current_accepted_user),
        session: Session = Depends(get_cloud_session),
) -> CurrentUserCryptoResponse:
    """Create exactly one opaque C11 wrapper set for the current normal user."""
    from .models import UserCrypto

    response.headers['Cache-Control'] = 'private, no-store'
    existing = session.get(UserCrypto, current.user.id)
    if existing is not None:
        if _same_crypto_record(existing, payload):
            response.status_code = status.HTTP_200_OK
            return account_crypto(response, current, session)
        raise _crypto_provisioning_conflict()

    password = payload.password
    recovery = payload.recovery
    record = UserCrypto(
        user_id=current.user.id,
        password_crypto_version=password.crypto_version,
        password_wrapping_version=password.wrapping_version,
        kdf_version=password.kdf.kdf_version,
        kdf_algorithm=password.kdf.algorithm,
        kdf_salt=decode_canonical_base64url(password.kdf.salt, expected_length=16),
        kdf_opslimit=password.kdf.opslimit,
        kdf_memlimit=password.kdf.memlimit,
        password_nonce=decode_canonical_base64url(password.nonce, expected_length=24),
        password_wrapped_amk=decode_canonical_base64url(password.ciphertext, expected_length=48),
        recovery_crypto_version=recovery.crypto_version,
        recovery_wrapping_version=recovery.wrapping_version,
        recovery_nonce=decode_canonical_base64url(recovery.nonce, expected_length=24),
        recovery_wrapped_amk=decode_canonical_base64url(recovery.ciphertext, expected_length=48),
    )
    try:
        session.add(record)
        session.commit()
    except IntegrityError:
        # The primary-key constraint is the authority for concurrent first
        # provisioning. A preflight read above is only a fast path.
        session.rollback()
        existing = session.get(UserCrypto, current.user.id)
        if existing is not None and _same_crypto_record(existing, payload):
            response.status_code = status.HTTP_200_OK
            return account_crypto(response, current, session)
        raise _crypto_provisioning_conflict() from None
    return account_crypto(response, current, session)


@router.get('/account/limits', response_model=AccountLimitsResponse)
def account_limits(current: AuthenticatedUser = Depends(get_current_accepted_user),
                   session: Session = Depends(get_cloud_session)) -> AccountLimitsResponse:
    try:
        limits = LimitsService().effective_for_user(session, current.user.id)
    except LimitsUnavailableError:
        raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail={
            'code': 'limits_unavailable', 'message': 'Limits are temporarily unavailable.',
        }) from None
    return AccountLimitsResponse(max_cloud_projects=limits.max_cloud_projects)


@router.post('/account/email/verification/request', status_code=status.HTTP_202_ACCEPTED)
def request_verification(background: BackgroundTasks, request: Request,
        current: AuthenticatedUser = Depends(get_current_user), session: Session = Depends(get_cloud_session),
        sender: EmailSender = Depends(get_email_sender)) -> dict[str, str]:
    token = _email_service(request).issue_verification(session, current.user)
    link = _trusted_link(request, '/verify-email', token) if token else None
    if token and link:
        background.add_task(_send, sender, OutgoingEmail(current.user.email, 'Подтверждение email',
            f'Подтвердите email: {link}\nСсылка действует 24 часа. Если это были не вы, проигнорируйте письмо.'))
    return {'code': 'verification_request_accepted'}


@router.post('/auth/email/verification/request', status_code=status.HTTP_202_ACCEPTED)
def request_public_verification(payload: PublicVerificationRequest, background: BackgroundTasks, request: Request,
        session: Session = Depends(get_cloud_session), sender: EmailSender = Depends(get_email_sender)) -> dict[str, str]:
    issued = _registration_service(request).issue_public_verification(session, payload.email)
    if issued is not None:
        email, token = issued
        link = _trusted_link(request, '/verify-email', token)
        if link:
            background.add_task(_send, sender, OutgoingEmail(email, 'Подтверждение email',
                f'Подтвердите email: {link}\nСсылка действует 24 часа. Если это были не вы, проигнорируйте письмо.'))
    return {'code': 'verification_request_accepted'}


@router.post('/auth/email/verify')
def verify_email(payload: VerificationTokenRequest, request: Request,
        session: Session = Depends(get_cloud_session)) -> dict[str, str]:
    try:
        result = _email_service(request).verify_email(session, payload.token)
    except RecoveryTokenError:
        raise _invalid_recovery_token() from None
    return {'code': 'email_verified', 'account_status': result.account_status, 'activation': result.activation}


@router.post('/auth/password-reset/request', status_code=status.HTTP_202_ACCEPTED)
def request_password_reset(payload: PasswordResetRequest, background: BackgroundTasks, request: Request,
        session: Session = Depends(get_cloud_session), sender: EmailSender = Depends(get_email_sender)) -> dict[str, str]:
    issued = _email_service(request).issue_password_reset(session, payload.email)
    if issued is not None:
        email, token = issued
        link = _trusted_link(request, '/reset-password', token, fragment=True)
        if link:
            background.add_task(_send, sender, OutgoingEmail(email, 'Восстановление пароля',
                f'Установите новый пароль: {link}\nСсылка действует 1 час. Если это были не вы, проигнорируйте письмо.'))
    return {'code': 'password_reset_request_accepted'}


@router.post('/auth/password-reset/confirm')
def confirm_password_reset(payload: PasswordResetConfirmRequest, background: BackgroundTasks, request: Request,
        session: Session = Depends(get_cloud_session), sender: EmailSender = Depends(get_email_sender)) -> dict[str, str]:
    try:
        email = _email_service(request).confirm_password_reset(session, payload.token, payload.new_password)
    except RecoveryTokenError:
        raise _invalid_recovery_token() from None
    except ValueError:
        raise HTTPException(status_code=422, detail={'code': 'invalid_password', 'message': 'Invalid password.'}) from None
    background.add_task(_send, sender, OutgoingEmail(email, 'Пароль изменён',
        'Пароль вашей учётной записи был изменён. Если это были не вы, обратитесь в поддержку.'))
    return {'code': 'password_reset_complete'}
