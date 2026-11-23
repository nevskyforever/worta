from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, Request, status
from sqlalchemy.orm import Session

from ..dependencies import AuthenticatedUser, get_cloud_session, get_current_accepted_user
from .schemas import (CloudProjectBootstrapCompletionRequest,
                      CloudProjectBootstrapDescriptor,
                      CloudProjectBootstrapListResponse,
                      CloudProjectBootstrapRegistrationRequest,
                      CloudProjectBootstrapResponse, CloudProjectsResponse)
from .services import (CloudProjectBootstrapError, CloudProjectLimitError, CloudProjectService,
                       LimitsUnavailableError)


router = APIRouter(prefix='/api/v1/cloud/projects', tags=['cloud projects'])


def _response(state) -> CloudProjectsResponse:
    return CloudProjectsResponse(
        cloud_project_ids=state.project_ids,
        cloud_project_count=state.count,
        max_cloud_projects=state.max_cloud_projects,
    )


def _limits_unavailable() -> HTTPException:
    return HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail={
        'code': 'limits_unavailable', 'message': 'Limits are temporarily unavailable.',
    })


def _bootstrap_descriptor(project) -> CloudProjectBootstrapDescriptor:
    return CloudProjectBootstrapDescriptor(
        project_id=project.project_id,
        bootstrap_id=project.bootstrap_id,
        origin_device_id=project.bootstrap_device_id,
        state=project.bootstrap_state,
        initial_event_count=project.initial_event_count,
        initial_max_server_sequence=project.initial_max_server_sequence,
    )


def _bootstrap_error(error: CloudProjectBootstrapError) -> HTTPException:
    return HTTPException(status_code=error.status_code, detail={
        'code': error.code, 'message': error.message,
    })


async def _reject_request_body(request: Request) -> None:
    """C8 never accepts a project payload, plaintext or otherwise."""
    if await request.body():
        raise HTTPException(status_code=status.HTTP_422_UNPROCESSABLE_CONTENT, detail={
            'code': 'cloud_project_content_not_accepted',
            'message': 'Cloud project content is not accepted by this API.',
        })


@router.get('', response_model=CloudProjectsResponse)
def list_cloud_projects(current: AuthenticatedUser = Depends(get_current_accepted_user),
                        session: Session = Depends(get_cloud_session)) -> CloudProjectsResponse:
    try:
        return _response(CloudProjectService().list(session, current.user.id))
    except LimitsUnavailableError:
        raise _limits_unavailable() from None


@router.get('/bootstrap', response_model=CloudProjectBootstrapListResponse)
def list_cloud_project_bootstraps(
    current: AuthenticatedUser = Depends(get_current_accepted_user),
    session: Session = Depends(get_cloud_session),
) -> CloudProjectBootstrapListResponse:
    state = CloudProjectService().list_bootstrap(session, current.user.id)
    return CloudProjectBootstrapListResponse(
        projects=[_bootstrap_descriptor(project) for project in state.projects],
        current_cursor=state.current_cursor,
    )


@router.post('/{project_id}/bootstrap', response_model=CloudProjectBootstrapResponse)
def register_cloud_project_bootstrap(
    project_id: str,
    request: CloudProjectBootstrapRegistrationRequest,
    current: AuthenticatedUser = Depends(get_current_accepted_user),
    session: Session = Depends(get_cloud_session),
) -> CloudProjectBootstrapResponse:
    if not project_id or len(project_id) > 512:
        raise HTTPException(status_code=422, detail={
            'code': 'invalid_project_id', 'message': 'Project ID is invalid.',
        })
    try:
        state = CloudProjectService().register_bootstrap(
            session, current.user.id, project_id, request.bootstrap_id, request.device_id,
        )
    except CloudProjectLimitError:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail={
            'code': 'cloud_project_limit_reached', 'message': 'Cloud project limit reached.',
        }) from None
    except LimitsUnavailableError:
        raise _limits_unavailable() from None
    except CloudProjectBootstrapError as error:
        raise _bootstrap_error(error) from None
    return CloudProjectBootstrapResponse(
        project=_bootstrap_descriptor(state.projects[0]), current_cursor=state.current_cursor,
    )


@router.post('/{project_id}/bootstrap/complete', response_model=CloudProjectBootstrapResponse)
def complete_cloud_project_bootstrap(
    project_id: str,
    request: CloudProjectBootstrapCompletionRequest,
    current: AuthenticatedUser = Depends(get_current_accepted_user),
    session: Session = Depends(get_cloud_session),
) -> CloudProjectBootstrapResponse:
    if not project_id or len(project_id) > 512:
        raise HTTPException(status_code=422, detail={
            'code': 'invalid_project_id', 'message': 'Project ID is invalid.',
        })
    try:
        state = CloudProjectService().complete_bootstrap(
            session, current.user.id, project_id, request.bootstrap_id, request.device_id,
            request.initial_event_count, request.initial_max_server_sequence,
        )
    except CloudProjectBootstrapError as error:
        raise _bootstrap_error(error) from None
    return CloudProjectBootstrapResponse(
        project=_bootstrap_descriptor(state.projects[0]), current_cursor=state.current_cursor,
    )


@router.post('/{project_id}', response_model=CloudProjectsResponse)
async def enable_cloud_project(project_id: str, request: Request,
                               current: AuthenticatedUser = Depends(get_current_accepted_user),
                               session: Session = Depends(get_cloud_session)) -> CloudProjectsResponse:
    if not project_id or len(project_id) > 512:
        raise HTTPException(status_code=422, detail={
            'code': 'invalid_project_id', 'message': 'Project ID is invalid.',
        })
    await _reject_request_body(request)
    try:
        return _response(CloudProjectService().enable(session, current.user.id, project_id))
    except CloudProjectBootstrapError as error:
        raise HTTPException(status_code=error.status_code, detail={'code': error.code, 'message': error.message}) from None
    except CloudProjectLimitError:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail={
            'code': 'cloud_project_limit_reached',
            'message': 'Cloud project limit reached.',
        }) from None
    except LimitsUnavailableError:
        raise _limits_unavailable() from None


@router.delete('/{project_id}', status_code=status.HTTP_204_NO_CONTENT)
def disable_cloud_project(project_id: str, current: AuthenticatedUser = Depends(get_current_accepted_user),
                          session: Session = Depends(get_cloud_session)) -> None:
    if not project_id or len(project_id) > 512:
        raise HTTPException(status_code=422, detail={
            'code': 'invalid_project_id', 'message': 'Project ID is invalid.',
        })
    try:
        CloudProjectService().disable(session, current.user.id, project_id)
    except CloudProjectBootstrapError as error:
        raise _bootstrap_error(error) from None
