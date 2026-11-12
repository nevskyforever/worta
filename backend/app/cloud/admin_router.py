from __future__ import annotations

from urllib.parse import quote
from uuid import UUID

from sqlalchemy import func, select
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from sqlalchemy.orm import Session

from ..dependencies import AuthenticatedUser, get_cloud_session, get_current_admin, get_email_sender
from .email import EmailDeliveryError, EmailSender, NullEmailSender, OutgoingEmail
from .invitations import InvitationError, PrivateInvitationService, invitation_status
from .models import GlobalLimits, RegistrationSettings, User
from .repositories import ReservedUsernameRepository, UserLimitOverridesRepository, UserRepository
from .schemas import (AdminInvitationCreateRequest, AdminInvitationResponse,
                      AdminLimitsPatch, AdminLimitsResponse, AdminRegistrationPatch,
                      AdminRegistrationResponse, AdminUserLimitPatch, AdminUserLimitResponse,
                      AdminUserResponse, AdminUsersResponse, ReservedUsernameRequest,
                      ReservedUsernameResponse)
from .services import AdminOperationError, AdminService, LimitsService, _UNSET
from .tokens import TokenService, utc_now

router = APIRouter(prefix='/api/v1/admin', tags=['cloud administration'])


def _error(error: AdminOperationError) -> HTTPException:
    return HTTPException(error.status_code, detail={'code': error.code, 'message': error.message})


def _user_response(session: Session, user: User) -> AdminUserResponse:
    override = UserLimitOverridesRepository().get(session, user.id)
    effective = LimitsService().effective_for_user(session, user.id).max_cloud_projects
    return AdminUserResponse(id=user.id, username=user.username, email=user.email,
        email_verified=user.email_verified, role=user.role, status=user.status,
        registration_mode_at_signup=user.registration_mode_at_signup, created_at=user.created_at,
        max_cloud_projects_override=(override.max_cloud_projects_override if override else None),
        effective_max_cloud_projects=effective)


def _invitation_response(row, user: User) -> AdminInvitationResponse:
    return AdminInvitationResponse(id=row.id, username=user.username, email=user.email,
        status=invitation_status(row, utc_now()), created_at=row.created_at,
        expires_at=row.expires_at)


@router.post('/invitations', response_model=AdminInvitationResponse, status_code=201)
def create_invitation(payload: AdminInvitationCreateRequest,
        request: Request, admin: AuthenticatedUser = Depends(get_current_admin),
        session: Session = Depends(get_cloud_session),
        sender: EmailSender = Depends(get_email_sender)) -> AdminInvitationResponse:
    base = request.app.state.runtime_config.public_web_url
    if base is None or isinstance(sender, NullEmailSender):
        raise HTTPException(503, detail={'code': 'invitation_delivery_unavailable'})
    service = PrivateInvitationService(TokenService(request.app.state.runtime_config.require_auth_secret()))
    try:
        row, raw = service.issue(session, admin_id=admin.user.id,
            username=payload.username, email=payload.email)
    except InvitationError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    link = f"{base.rstrip('/')}/invite#token={quote(raw, safe='')}"
    try:
        sender.send(OutgoingEmail(payload.email, 'Приглашение в worta',
            f'Администратор пригласил вас в закрытое тестирование worta.\n'
            f'Создайте свой пароль по ссылке: {link}\n'
            'Ссылка действует 72 часа и используется один раз.'))
    except EmailDeliveryError:
        service.revoke(session, row.id)
        raise HTTPException(503, detail={'code': 'invitation_delivery_unavailable'}) from None
    user = session.get(User, row.user_id)
    return _invitation_response(row, user)


@router.get('/invitations', response_model=list[AdminInvitationResponse])
def list_invitations(request: Request, limit: int = Query(100, ge=1, le=100),
        _admin: AuthenticatedUser = Depends(get_current_admin),
        session: Session = Depends(get_cloud_session)) -> list[AdminInvitationResponse]:
    service = PrivateInvitationService(TokenService(request.app.state.runtime_config.require_auth_secret()))
    return [_invitation_response(row, user) for row, user in service.list(session, limit=limit)]


@router.post('/invitations/{invitation_id}/revoke', response_model=AdminInvitationResponse)
def revoke_invitation(invitation_id: UUID, request: Request,
        _admin: AuthenticatedUser = Depends(get_current_admin),
        session: Session = Depends(get_cloud_session)) -> AdminInvitationResponse:
    service = PrivateInvitationService(TokenService(request.app.state.runtime_config.require_auth_secret()))
    try:
        row = service.revoke(session, invitation_id)
    except InvitationError as error:
        raise HTTPException(error.status_code, detail={'code': error.code}) from None
    return _invitation_response(row, session.get(User, row.user_id))


@router.get('/users', response_model=AdminUsersResponse)
def users(limit: int = Query(50, ge=1, le=100), offset: int = Query(0, ge=0),
          status_filter: str | None = Query(None, alias='status'), role: str | None = None,
          search: str | None = Query(None, max_length=320),
          _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminUsersResponse:
    rows, total = UserRepository().list_admin(session, limit=limit, offset=offset,
        status=status_filter, role=role, search=search)
    return AdminUsersResponse(users=[_user_response(session, row) for row in rows], total=total, limit=limit, offset=offset)


def _lifecycle(method: str):
    def endpoint(user_id: UUID, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminUserResponse:
        try:
            return _user_response(session, getattr(AdminService(), method)(session, user_id))
        except AdminOperationError as error:
            raise _error(error) from None
    return endpoint


router.post('/users/{user_id}/approve', response_model=AdminUserResponse)(_lifecycle('approve'))
router.post('/users/{user_id}/reject', response_model=AdminUserResponse)(_lifecycle('reject'))
router.post('/users/{user_id}/block', response_model=AdminUserResponse)(_lifecycle('block'))
router.post('/users/{user_id}/unblock', response_model=AdminUserResponse)(_lifecycle('unblock'))


@router.post('/users/{user_id}/sessions/revoke')
def revoke_sessions(user_id: UUID, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> dict[str, int | str]:
    try:
        sessions, tokens = AdminService().revoke_sessions(session, user_id)
    except AdminOperationError as error:
        raise _error(error) from None
    return {'code': 'sessions_revoked', 'sessions': sessions, 'refresh_tokens': tokens}


@router.get('/registration', response_model=AdminRegistrationResponse)
def registration(_admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminRegistrationResponse:
    settings = session.get(RegistrationSettings, 1)
    if settings is None:
        raise HTTPException(503, detail={'code': 'registration_unavailable', 'message': 'Registration settings are unavailable.'})
    active = session.scalar(select(func.count()).select_from(User).where(User.status == 'active')) or 0
    return AdminRegistrationResponse(mode=settings.mode, max_users=settings.max_users, active_users=active)


@router.patch('/registration', response_model=AdminRegistrationResponse)
def patch_registration(payload: AdminRegistrationPatch, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminRegistrationResponse:
    try:
        settings = AdminService().update_registration(session, mode=payload.mode,
            max_users=payload.max_users if 'max_users' in payload.model_fields_set else _UNSET)
    except AdminOperationError as error:
        raise _error(error) from None
    active = session.scalar(select(func.count()).select_from(User).where(User.status == 'active')) or 0
    return AdminRegistrationResponse(mode=settings.mode, max_users=settings.max_users, active_users=active)


@router.get('/limits', response_model=AdminLimitsResponse)
def limits(_admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminLimitsResponse:
    row = session.get(GlobalLimits, 1)
    if row is None:
        raise HTTPException(503, detail={'code': 'limits_unavailable', 'message': 'Limits are unavailable.'})
    return AdminLimitsResponse(max_cloud_projects=row.max_cloud_projects)


@router.patch('/limits', response_model=AdminLimitsResponse)
def patch_limits(payload: AdminLimitsPatch, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminLimitsResponse:
    try:
        row = AdminService().update_global_limits(session, payload.max_cloud_projects)
    except AdminOperationError as error:
        raise _error(error) from None
    return AdminLimitsResponse(max_cloud_projects=row.max_cloud_projects)


@router.patch('/users/{user_id}/limits', response_model=AdminUserLimitResponse)
def patch_user_limits(user_id: UUID, payload: AdminUserLimitPatch, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> AdminUserLimitResponse:
    try:
        override, effective = AdminService().update_override(session, user_id, payload.max_cloud_projects_override)
    except AdminOperationError as error:
        raise _error(error) from None
    return AdminUserLimitResponse(max_cloud_projects_override=override, effective_max_cloud_projects=effective)


@router.get('/reserved-usernames', response_model=list[ReservedUsernameResponse])
def reserved(_admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> list[ReservedUsernameResponse]:
    return [ReservedUsernameResponse(username_normalized=row.username_normalized, created_at=row.created_at)
            for row in ReservedUsernameRepository().list(session)]


@router.post('/reserved-usernames', response_model=ReservedUsernameResponse)
def add_reserved(payload: ReservedUsernameRequest, _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> ReservedUsernameResponse:
    try:
        row = AdminService().add_reserved_username(session, payload.username)
    except AdminOperationError as error:
        raise _error(error) from None
    return ReservedUsernameResponse(username_normalized=row.username_normalized, created_at=row.created_at)


@router.delete('/reserved-usernames')
def delete_reserved(username: str = Query(min_length=1, max_length=128), _admin: AuthenticatedUser = Depends(get_current_admin), session: Session = Depends(get_cloud_session)) -> dict[str, str]:
    AdminService().remove_reserved_username(session, username)
    return {'code': 'reserved_username_removed'}
