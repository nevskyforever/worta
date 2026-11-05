from __future__ import annotations

import asyncio
import logging
import os
from contextlib import asynccontextmanager

from fastapi import Depends, FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse

import engine
from nfprogress.core.errors import DomainError
from nfprogress.core.repositories.storage import PickleRepository
from nfprogress.core.services.content import ContentService
from nfprogress.core.services.game import GameService
from nfprogress.core.services.integrations import DocumentIntegrationService
from nfprogress.core.services.documents import ProjectDocumentService
from nfprogress.core.services.notes import ProjectNotesService
from nfprogress.core.services.projects import ProjectService
from nfprogress.core.services.settings import SettingsService
from nfprogress.core.sqlite import (
    StorageOwner, StorageOwnershipRepository, Subsystem, cutover_notes,
    cutover_settings,
)
from nfprogress.core.migration import cutover_game, cutover_projects

from .config import RuntimeConfig
from .db import CloudDatabase, DatabaseReadiness
from .dependencies import Services, require_session
from .cloud.router import router as cloud_router
from .cloud.admin_router import router as cloud_admin_router
from .cloud.projects_router import router as cloud_projects_router
from .cloud.sync_router import router as cloud_sync_router
from .cloud.sync_v2_router import router as cloud_sync_v2_router
from .cloud.sync_v3_router import router as cloud_sync_v3_router, recovery_router as cloud_recovery_router
from .cloud.encrypted_blobs_router import router as cloud_encrypted_blobs_router
from .cloud.email import email_sender_from_config
from .cloud.passwords import PasswordCapacityError
from .routers import content, documents, game, integrations, notes, projects


_LOGGER = logging.getLogger(__name__)


def _desktop_sync_state(services: Services) -> tuple[bool, object]:
    repository = services.repository
    locked = getattr(repository, 'locked', None)
    if callable(locked):
        with locked():
            settings = engine.load_settings()
            return bool(settings.get('background_synch', True)), engine.today_for_test()
    settings = repository.read_settings()
    return bool(settings.get('background_synch', True)), engine.today_for_test()


def _desktop_game_enabled(services: Services) -> bool:
    return bool(services.repository.read_settings().get('game_mode', False))


async def _desktop_sync_loop(services: Services) -> None:
    previous_day = None
    was_enabled = False
    while True:
        try:
            # Day boundaries must be processed even when no document source is
            # configured. This applies automatic local/global freezes and keeps
            # both streak displays current in a running desktop application.
            projects_owned_by_sqlite = (
                StorageOwnershipRepository(services.repository.base_dir).get_owner(
                    Subsystem.PROJECTS,
                ) == StorageOwner.SQLITE
            )
            if not projects_owned_by_sqlite:
                await asyncio.to_thread(services.projects.refresh_streak_statuses)
            enabled, current_day = await asyncio.to_thread(
                _desktop_sync_state, services,
            )
            is_new_writing_day = not was_enabled or current_day != previous_day
            if enabled and is_new_writing_day and not projects_owned_by_sqlite:
                await asyncio.to_thread(services.integrations.sync_all_configured)
            # The legacy UI also settles deposits, loan payments and their
            # notifications when its clock crosses into a new writing day.
            # Keep those persisted transitions alive even if the Game page is
            # never opened in this desktop session.
            if is_new_writing_day and await asyncio.to_thread(
                    _desktop_game_enabled, services,
            ):
                await asyncio.to_thread(services.game.process_bank_events)
            previous_day = current_day
            was_enabled = enabled
        except asyncio.CancelledError:
            raise
        except Exception:
            _LOGGER.exception('Desktop background synchronization failed.')
        await asyncio.sleep(60)


def _legacy_services(runtime_config: RuntimeConfig) -> Services:
    data_dir = runtime_config.data_dir
    if data_dir is None:
        data_dir = (
            engine.get_test_data_dir() if engine.dev_mode
            else engine.get_app_data_dir()
        )
    repository = PickleRepository(data_dir)
    if StorageOwnershipRepository(data_dir).get_owner(Subsystem.SETTINGS) == StorageOwner.PICKLE:
        try:
            with repository.locked():
                cutover_settings(data_dir, engine.load_settings())
        except Exception:
            _LOGGER.exception('Settings SQLite cutover failed; keeping pickle ownership.')
    projects_owned_by_sqlite = (
        StorageOwnershipRepository(data_dir).get_owner(Subsystem.PROJECTS)
        == StorageOwner.SQLITE
    )
    # The Web compatibility profile keeps Projects in pickle. Its Notes must
    # stay there too: the SQLite Notes table has foreign keys to SQLite project
    # rows, which are intentionally absent from this profile.
    if (
        StorageOwnershipRepository(data_dir).get_owner(Subsystem.NOTES)
        == StorageOwner.PICKLE
        and (runtime_config.platform != 'web' or projects_owned_by_sqlite)
    ):
        try:
            with repository.locked():
                cutover_notes(data_dir, engine.load_data())
        except Exception:
            _LOGGER.exception('Notes SQLite cutover failed; keeping pickle ownership.')
    # Only the Tauri-spawned sidecar performs the local desktop ownership
    # switch. Direct ``create_app(..., platform='desktop')`` remains a
    # compatibility harness for non-migrated backend routes.
    if (
        runtime_config.platform == 'desktop'
        and os.environ.get('NFPROGRESS_TAURI_RUNTIME') == '1'
        and StorageOwnershipRepository(data_dir).get_owner(Subsystem.PROJECTS) == StorageOwner.PICKLE
    ):
        try:
            cutover_projects(data_dir)
        except Exception:
            _LOGGER.exception('Projects SQLite cutover failed; keeping pickle ownership.')
    # F3 performs the Game owner switch only for the Tauri desktop process.
    # Web and compatibility harnesses keep their independent legacy authority.
    if (
        runtime_config.platform == 'desktop'
        and os.environ.get('NFPROGRESS_TAURI_RUNTIME') == '1'
        and StorageOwnershipRepository(data_dir).get_owner(Subsystem.GAME) == StorageOwner.PICKLE
    ):
        try:
            cutover_game(data_dir)
        except Exception:
            _LOGGER.exception('Game SQLite cutover failed; keeping pickle ownership.')
    game_service = GameService(
        repository, developer_mode=runtime_config.developer_mode,
    )
    if runtime_config.platform == 'desktop':
        try:
            game_service.process_pending_events()
        except Exception:
            _LOGGER.exception('Pending Game events could not be processed at startup.')
    project_service = ProjectService(repository, game_service=game_service)
    return Services(
        repository=repository,
        projects=project_service,
        notes_class=ProjectNotesService,
        game=game_service,
        settings=SettingsService(
            repository,
            platform=runtime_config.platform,
            developer_mode=runtime_config.developer_mode,
        ),
        content=ContentService(),
        integrations=DocumentIntegrationService(
            repository,
            project_service,
            allow_local_files=runtime_config.allow_local_files,
        ),
        documents=ProjectDocumentService(
            repository, project_service,
            allow_local_files=runtime_config.allow_local_files,
        ),
    )


def create_app(config: RuntimeConfig | None = None) -> FastAPI:
    runtime_config = config or RuntimeConfig.from_env()
    if runtime_config.environment == 'test' and (
        runtime_config.data_dir is None
        or (config is None and not (os.environ.get('NFPROGRESS_DATA_DIR') or '').strip())
    ):
        raise ValueError('NFPROGRESS_DATA_DIR is required in test mode.')
    cloud_database = CloudDatabase(runtime_config.database_url)
    services = None if runtime_config.cloud_only else _legacy_services(runtime_config)

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        sync_task = None
        if runtime_config.platform == 'desktop' and services is not None:
            sync_task = asyncio.create_task(_desktop_sync_loop(services))
        try:
            yield
        finally:
            if sync_task is not None:
                sync_task.cancel()
                try:
                    await sync_task
                except asyncio.CancelledError:
                    pass
            cloud_database.dispose()

    app = FastAPI(
        title='nfprogress API',
        version=engine.version,
        description='Shared API for nfprogress Web, Tauri, and Capacitor clients.',
        lifespan=lifespan,
    )
    app.state.runtime_config = runtime_config
    app.state.services = services
    app.state.cloud_database = cloud_database
    app.state.email_sender = email_sender_from_config(runtime_config)
    app.add_middleware(
        CORSMiddleware,
        allow_origins=list(runtime_config.allowed_origins),
        allow_credentials=False,
        allow_methods=['GET', 'POST', 'PUT', 'PATCH', 'DELETE'],
        allow_headers=['Content-Type', 'X-NFProgress-Token', 'Authorization',
                       'X-WORTA-Crypto-Version', 'X-WORTA-AAD-Version', 'X-WORTA-Nonce'],
        expose_headers=['X-WORTA-Crypto-Version', 'X-WORTA-AAD-Version', 'X-WORTA-Nonce'],
    )

    @app.exception_handler(DomainError)
    async def handle_domain_error(_request: Request, error: DomainError):
        return JSONResponse(
            status_code=error.status_code,
            content={'detail': {'code': error.code, 'message': error.message}},
        )

    @app.exception_handler(PasswordCapacityError)
    async def handle_password_capacity(_request: Request, _error: PasswordCapacityError):
        return JSONResponse(status_code=503, headers={'Retry-After': '2'},
            content={'detail': {'code': 'password_capacity_unavailable'}})

    @app.exception_handler(RequestValidationError)
    async def handle_request_validation(
            _request: Request, error: RequestValidationError,
    ):
        fields = [
            {
                'field': '.'.join(str(part) for part in item['loc']),
                'message': item['msg'],
                'type': item['type'],
            }
            for item in error.errors()
        ]
        return JSONResponse(
            status_code=422,
            content={
                'detail': {
                    'code': 'invalid_request',
                    'message': 'Запрос содержит неверные данные.',
                    'fields': fields,
                },
            },
        )

    @app.get('/health', tags=['system'])
    def health():
        return {'status': 'ok', 'version': engine.version}

    @app.get('/ready', tags=['system'])
    def ready():
        readiness = cloud_database.readiness()
        if readiness == DatabaseReadiness.READY:
            return {'status': 'ok', 'database': readiness.value}
        if readiness == DatabaseReadiness.NOT_CONFIGURED:
            return JSONResponse(
                status_code=503,
                content={'status': 'not_ready', 'database': readiness.value},
            )
        return JSONResponse(
            status_code=503,
            content={'status': 'not_ready', 'database': readiness.value},
        )

    if services is not None:
        api_dependencies = [Depends(require_session)]
        app.include_router(projects.router, prefix='/api', dependencies=api_dependencies)
        app.include_router(notes.router, prefix='/api', dependencies=api_dependencies)
        app.include_router(game.router, prefix='/api', dependencies=api_dependencies)
        app.include_router(content.router, prefix='/api', dependencies=api_dependencies)
        app.include_router(integrations.router, prefix='/api', dependencies=api_dependencies)
        app.include_router(documents.router, prefix='/api', dependencies=api_dependencies)
    app.include_router(cloud_router)
    app.include_router(cloud_projects_router)
    app.include_router(cloud_sync_router)
    app.include_router(cloud_sync_v2_router)
    app.include_router(cloud_sync_v3_router)
    app.include_router(cloud_recovery_router)
    app.include_router(cloud_encrypted_blobs_router)
    app.include_router(cloud_admin_router)
    return app


app = create_app()
