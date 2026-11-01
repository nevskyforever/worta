import json
import math
import os
import pickle
import platform
import shutil
import sys
import tempfile
import uuid
from contextlib import contextmanager
from contextvars import ContextVar
from copy import deepcopy
from datetime import datetime, timedelta, date, date as date_type, time
from pathlib import Path
from collections import defaultdict


MAX_PROJECT_COVER_IMAGE_LENGTH = 5_000_000


# Режим разработчика.
dev_mode = "__compiled__" not in globals()

# Версия приложения
version = '5.3.11'

# Определяем систему

SYSTEM = platform.system()  # 'Windows', 'Darwin' (macOS), 'Linux'


_explicit_data_dir = ContextVar('nfprogress_explicit_data_dir', default=None)


@contextmanager
def data_directory_context(base_dir):
    """Temporarily route persistence to an explicit directory.

    The override is local to the current execution context, so a backend request
    can safely use isolated storage without changing the desktop application's
    process-wide default data directory.
    """
    data_dir = Path(base_dir)
    data_dir.mkdir(parents=True, exist_ok=True)
    token = _explicit_data_dir.set(data_dir)
    try:
        yield data_dir
    finally:
        _explicit_data_dir.reset(token)


def _copy_missing_user_data(source_dir, destination_dir):
    """Копирует старые пользовательские данные без удаления и перезаписи"""
    source_dir = Path(source_dir)
    destination_dir = Path(destination_dir)
    if not source_dir.is_dir() or source_dir.resolve() == destination_dir.resolve():
        return []

    copied = []
    for source in source_dir.rglob('*'):
        if not source.is_file() or source.is_symlink():
            continue
        relative_path = source.relative_to(source_dir)
        destination = destination_dir / relative_path
        if destination.exists():
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = destination.with_name(destination.name + '.migration.tmp')
        try:
            shutil.copy2(source, temporary)
            if temporary.stat().st_size != source.stat().st_size:
                raise OSError(f'Размер скопированного файла не совпадает: {source}')
            temporary.replace(destination)
        except Exception:
            temporary.unlink(missing_ok=True)
            raise
        copied.append(destination)
    return copied


def _migrate_windows_user_data(destination_dir):
    user_profile = Path(os.environ.get('USERPROFILE') or Path.home())
    legacy_directories = [user_profile / 'Documents' / 'nfprogress']
    copied = []
    for legacy_dir in legacy_directories:
        copied.extend(_copy_missing_user_data(legacy_dir, destination_dir))
    return copied

def get_app_data_dir():
    """
    Возвращает путь к директории для хранения данных приложения
    в зависимости от операционной системы
    """
    if SYSTEM == 'Windows':
        app_data = os.environ.get('APPDATA')
        base_dir = Path(app_data) if app_data else Path.home() / 'AppData' / 'Roaming'
    elif SYSTEM == 'Darwin':  # macOS
        # macOS: /Users/<USER>/Documents/MyAppData
        base_dir = Path.home() / 'Documents'
    else:  # Linux и другие
        # Linux: /home/<USER>/Documents или ~/.local/share/MyApp
        base_dir = Path.home() / 'Documents'
        # Если папки Documents нет, используем стандартную директорию для данных
        if not base_dir.exists():
            base_dir = Path.home() / '.local' / 'share'

    # Создаем папку для нашего приложения
    app_data_dir = base_dir / 'nfprogress'

    # Создаем директорию, если её нет
    try:
        app_data_dir.mkdir(parents=True, exist_ok=True)
    except PermissionError:
        # Если нет прав на запись в Documents, используем домашнюю папку
        app_data_dir = Path.home() / '.nfprogress'
        app_data_dir.mkdir(parents=True, exist_ok=True)

    if SYSTEM == 'Windows':
        try:
            _migrate_windows_user_data(app_data_dir)
        except OSError as error:
            try:
                with (app_data_dir / 'migration.log').open('a', encoding='utf-8') as log_file:
                    log_file.write(f'{datetime.now().isoformat()} migration failed: {error}\n')
            except OSError as log_error:
                print(f'Не удалось записать журнал миграции: {log_error}', file=sys.stderr)

    return app_data_dir


def get_test_data_dir():
    """Возвращает путь к директории тестовых данных и создаёт её при необходимости."""
    test_dir = get_app_data_dir() / 'test_data'
    test_dir.mkdir(parents=True, exist_ok=True)
    return test_dir


def sync_test_data(destination_dir=None, *, replace=True):
    """Копирует authoritative legacy-файлы в тестовую директорию.

    Эта низкоуровневая функция не копирует ``nfprogress.db``. Для подготовки
    полного Tauri-профиля используется :func:`refresh_test_data`.
    """
    source_dir = get_app_data_dir()
    test_dir = Path(destination_dir) if destination_dir is not None else get_test_data_dir()
    test_dir.mkdir(parents=True, exist_ok=True)
    for pattern in ('*.pkl', 'documents.json'):
        for data_file in source_dir.glob(pattern):
            test_file = test_dir / data_file.name
            if test_file.exists() and not replace:
                continue
            shutil.copy2(data_file, test_file)


def refresh_test_data():
    """Atomically prepare canonical test data from current real legacy data."""
    from nfprogress.migration_helper import refresh_test_data_profile

    return refresh_test_data_profile(get_app_data_dir(), get_test_data_dir())


def get_data_file_path(name):
    """Возвращает путь к файлу данных.

    В режиме разработчика файлы читаются и пишутся из папки test_data.
    """
    explicit_data_dir = _explicit_data_dir.get()
    if explicit_data_dir is not None:
        return explicit_data_dir / f'{name}.pkl'
    if dev_mode:
        return get_test_data_dir() / f'{name}.pkl'
    return get_app_data_dir() / f'{name}.pkl'


def resource_path(relative_path):
    """Получить путь к ресурсу, работает и в .py, и в .app, и в .exe"""
    if hasattr(sys, '_MEIPASS'):
        # PyInstaller создает временную папку и хранит путь в _MEIPASS
        return os.path.join(sys._MEIPASS, relative_path)
    return str(Path(__file__).resolve().parent / relative_path)


def _mindmap_data_has_valid_root(value):
    if value is None:
        return False
    if not isinstance(value, dict):
        return False

    node_data = value.get('nodeData')
    if not isinstance(node_data, dict):
        return False
    if not isinstance(node_data.get('id'), str) or not node_data['id']:
        return False
    if not isinstance(node_data.get('topic'), str):
        return False
    if not isinstance(node_data.get('children'), list):
        return False
    return True


def normalize_mindmap_data(value):
    """Return JSON-safe Mind Elixir data or ``None`` for malformed input."""
    if not _mindmap_data_has_valid_root(value):
        return None

    try:
        serialized = json.dumps(value, ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError):
        return None
    normalized = json.loads(serialized)
    free_nodes = normalized.get('freeNodes')
    if free_nodes is not None:
        if not isinstance(free_nodes, list):
            normalized.pop('freeNodes', None)
        else:
            normalized['freeNodes'] = [
                node
                for item in free_nodes
                if (node := _normalize_free_mindmap_node(item)) is not None
            ]
    floating_items = normalized.get('nfprogressFloatingItems')
    if floating_items is not None:
        if not isinstance(floating_items, list):
            normalized.pop('nfprogressFloatingItems', None)
        else:
            normalized_items = [
                {
                    'id': item['id'],
                    'kind': item['kind'],
                    'text': item['text'],
                    'x': min(100, max(0, item['x'])),
                    'y': min(100, max(0, item['y'])),
                    'parentId': item.get('parentId'),
                }
                for item in floating_items
                if (
                    isinstance(item, dict)
                    and isinstance(item.get('id'), str)
                    and item['id']
                    and item.get('kind') in {'node', 'note'}
                    and isinstance(item.get('text'), str)
                    and isinstance(item.get('x'), (int, float))
                    and not isinstance(item['x'], bool)
                    and isinstance(item.get('y'), (int, float))
                    and not isinstance(item['y'], bool)
                )
            ]
            item_ids = {item['id'] for item in normalized_items}
            for item in normalized_items:
                if (
                    item['kind'] != 'node'
                    or not isinstance(item['parentId'], str)
                    or item['parentId'] not in item_ids
                    or item['parentId'] == item['id']
                ):
                    item.pop('parentId')
            normalized['nfprogressFloatingItems'] = normalized_items
    floating_links = normalized.get('nfprogressFloatingLinks')
    if floating_links is not None:
        if not isinstance(floating_links, list):
            normalized.pop('nfprogressFloatingLinks', None)
        else:
            normalized['nfprogressFloatingLinks'] = [
                {
                    'id': link['id'],
                    'fromType': link['fromType'],
                    'from': link['from'],
                    'toType': link['toType'],
                    'to': link['to'],
                }
                for link in floating_links
                if (
                    isinstance(link, dict)
                    and isinstance(link.get('id'), str)
                    and link['id']
                    and link.get('fromType') in {'floating', 'node'}
                    and isinstance(link.get('from'), str)
                    and link['from']
                    and link.get('toType') in {'floating', 'node'}
                    and isinstance(link.get('to'), str)
                    and link['to']
                    and not (
                        link['fromType'] == link['toType']
                        and link['from'] == link['to']
                    )
                )
            ]
    return normalized


def normalize_project_cover_image(value):
    """Return a safe, bounded cover data URL or ``None``.

    Covers are produced by the frontend cropper as JPEG data URLs.  PNG and
    WebP are accepted as well to keep saves made by compatible clients valid.
    """
    if not isinstance(value, str) or len(value) > MAX_PROJECT_COVER_IMAGE_LENGTH:
        return None
    prefix, separator, encoded = value.partition(',')
    if (
            not separator
            or prefix not in {
                'data:image/jpeg;base64',
                'data:image/png;base64',
                'data:image/webp;base64',
            }
            or not encoded
    ):
        return None
    try:
        import base64
        base64.b64decode(encoded, validate=True)
    except (ValueError, TypeError):
        return None
    return value


def _normalize_free_mindmap_node(value, *, is_root=True):
    if (
        not isinstance(value, dict)
        or not isinstance(value.get('id'), str)
        or not value['id']
        or not isinstance(value.get('topic'), str)
    ):
        return None

    node = deepcopy(value)
    children = value.get('children', [])
    if not isinstance(children, list):
        children = []
    node['children'] = [
        child
        for item in children
        if (child := _normalize_free_mindmap_node(item, is_root=False)) is not None
    ]
    if is_root:
        position = value.get('position')
        if not isinstance(position, dict):
            position = {}
        x = position.get('x', 320)
        y = position.get('y', 240)
        node['position'] = {
            'x': x if isinstance(x, (int, float)) and not isinstance(x, bool) else 320,
            'y': y if isinstance(y, (int, float)) and not isinstance(y, bool) else 240,
        }
        node['nfprogressFreeRoot'] = True
    else:
        node.pop('position', None)
        node.pop('nfprogressFreeRoot', None)
    if not isinstance(node.get('nfprogressNote'), bool):
        node.pop('nfprogressNote', None)
    node.pop('parent', None)
    return node


_MINDMAP_STAGE_ID_KEY = 'nfprogressStageId'
_MINDMAP_STAGE_ROOT_KEY = 'nfprogressStageRoot'
_MINDMAP_SOURCE_ID_KEY = 'nfprogressSourceId'
_MINDMAP_READ_ONLY_KEY = 'nfprogressReadOnly'
_MINDMAP_EMPTY_STAGE_KEY = 'nfprogressEmptyStageMap'
_MINDMAP_INTERNAL_KEYS = {
    _MINDMAP_STAGE_ID_KEY,
    _MINDMAP_STAGE_ROOT_KEY,
    _MINDMAP_SOURCE_ID_KEY,
    _MINDMAP_READ_ONLY_KEY,
    _MINDMAP_EMPTY_STAGE_KEY,
}


PROJECT_NOTE_COLORS = {
    'default',
    'coral',
    'orange',
    'yellow',
    'green',
    'teal',
    'blue',
    'purple',
    'pink',
    'brown',
    'gray',
}


def normalize_project_note_records(value):
    """Return save-compatible project-note records and discard malformed rows."""
    if not isinstance(value, list):
        return []

    normalized = []
    seen_ids = set()
    seen_sources = set()
    for index, raw_record in enumerate(value):
        if not isinstance(raw_record, dict):
            continue
        note_id = raw_record.get('id')
        if not isinstance(note_id, str) or not note_id or note_id in seen_ids:
            continue

        source_type = raw_record.get('source_type', 'project')
        if source_type not in {'project', 'mindmap'}:
            source_type = 'project'
        source_node_id = raw_record.get('source_node_id')
        source_map_id = raw_record.get('source_map_id')
        if source_type == 'mindmap':
            if not isinstance(source_node_id, str) or not source_node_id:
                continue
            if source_node_id in seen_sources:
                continue
            seen_sources.add(source_node_id)
            if not isinstance(source_map_id, str) or not source_map_id:
                source_map_id = None
        else:
            source_node_id = None
            source_map_id = None

        title = raw_record.get('title', '')
        content = raw_record.get('content', '')
        created_at = raw_record.get('created_at', '')
        updated_at = raw_record.get('updated_at', created_at)
        color = raw_record.get('color', 'default')
        sort_order = raw_record.get('sort_order', index)
        revision = raw_record.get('revision', 0)
        if not isinstance(title, str):
            title = str(title) if title is not None else ''
        if not isinstance(content, str):
            content = str(content) if content is not None else ''
        title = title.replace('\x00', '')
        content = content.replace('\x00', '')
        if not isinstance(created_at, str):
            created_at = ''
        if not isinstance(updated_at, str):
            updated_at = created_at
        if color not in PROJECT_NOTE_COLORS:
            color = 'default'
        if not isinstance(sort_order, int) or isinstance(sort_order, bool):
            sort_order = index
        if not isinstance(revision, int) or isinstance(revision, bool) or revision < 0:
            revision = 0

        tags = []
        seen_tags = set()
        raw_tags = raw_record.get('tags', [])
        for raw_tag in raw_tags if isinstance(raw_tags, list) else []:
            if not isinstance(raw_tag, str):
                continue
            tag = raw_tag.replace('\x00', '').strip().lstrip('#').strip()[:64]
            tag_key = tag.casefold()
            if not tag or tag_key == 'карта' or tag_key in seen_tags:
                continue
            seen_tags.add(tag_key)
            tags.append(tag)

        checklist = []
        seen_checklist_ids = set()
        if source_type == 'project':
            raw_checklist = raw_record.get('checklist', [])
            for raw_item in raw_checklist if isinstance(raw_checklist, list) else []:
                if not isinstance(raw_item, dict):
                    continue
                item_id = raw_item.get('id')
                item_text = raw_item.get('text')
                if (
                        not isinstance(item_id, str)
                        or not item_id
                        or item_id in seen_checklist_ids
                ):
                    continue
                if not isinstance(item_text, str):
                    item_text = str(item_text) if item_text is not None else ''
                checklist.append({
                    'id': item_id,
                    'text': item_text.replace('\x00', '')[:2000],
                    'checked': bool(raw_item.get('checked', False)),
                })
                seen_checklist_ids.add(item_id)

        normalized.append({
            'id': note_id,
            'title': title[:500],
            # Mind-map text remains canonical in mindmap_data; do not duplicate it.
            'content': content[:300_000] if source_type == 'project' else '',
            'content_format': 'html' if source_type == 'project' else 'plain',
            'checklist': checklist,
            'color': color,
            'pinned': bool(raw_record.get('pinned', False)),
            'archived': bool(raw_record.get('archived', False)),
            'sort_order': max(0, sort_order),
            'tags': tags,
            'source_type': source_type,
            'source_map_id': source_map_id,
            'source_node_id': source_node_id,
            'created_at': created_at,
            'updated_at': updated_at,
            'revision': revision,
        })
        seen_ids.add(note_id)
    return normalized


def mindmap_has_content(value, root_topic):
    """Return whether a map contains more than its untouched default root."""
    normalized = normalize_mindmap_data(value)
    if normalized is None:
        return False

    root = normalized['nodeData']
    if root.get('topic') != root_topic:
        return True
    if root.get('children'):
        return True
    if normalized.get('arrows') or normalized.get('summaries'):
        return True
    if (
        normalized.get('nfprogressFloatingItems')
        or normalized.get('nfprogressFloatingLinks')
        or normalized.get('freeNodes')
    ):
        return True

    default_root_keys = {
        'id',
        'topic',
        'children',
        'root',
        'expanded',
        'direction',
    }
    return any(
        value not in (None, '', [], {})
        for key, value in root.items()
        if key not in default_root_keys
    )


def _new_mindmap_data(root_topic, root_id):
    return {
        'nodeData': {
            'id': root_id,
            'topic': root_topic,
            'children': [],
        },
    }


def _valid_mindmap_id(value, fallback):
    return value if isinstance(value, str) and value else fallback


def _combined_stage_node_id(stage_id, source_id, is_root=False):
    if is_root:
        return f'nfprogress-stage-{stage_id}'
    return f'nfprogress-stage-{stage_id}-{source_id}'


def _combined_stage_item_id(stage_id, source_id, item_type):
    return f'nfprogress-{item_type}-{stage_id}-{source_id}'


def _strip_mindmap_internal_keys(value):
    cleaned = deepcopy(value)
    if isinstance(cleaned, dict):
        for key in _MINDMAP_INTERNAL_KEYS:
            cleaned.pop(key, None)
    return cleaned


def _copy_project_mindmap_node(node):
    """Copy a project-owned node while removing generated stage branches."""
    if not isinstance(node, dict):
        return None
    if node.get(_MINDMAP_STAGE_ROOT_KEY):
        return None

    copied = _strip_mindmap_internal_keys(node)
    copied.pop('parent', None)
    copied['children'] = []
    for child in node.get('children', []):
        copied_child = _copy_project_mindmap_node(child)
        if copied_child is not None:
            copied['children'].append(copied_child)
    return copied


def _copy_stage_mindmap_node(
        node,
        stage_id,
        id_map,
        is_root=False,
        read_only=False,
):
    if not isinstance(node, dict):
        return None

    fallback_id = uuid.uuid4().hex
    source_id = _valid_mindmap_id(node.get('id'), fallback_id)
    combined_id = _combined_stage_node_id(stage_id, source_id, is_root)
    id_map[source_id] = combined_id

    copied = _strip_mindmap_internal_keys(node)
    copied.pop('parent', None)
    copied.pop('root', None)
    copied['id'] = combined_id
    copied[_MINDMAP_STAGE_ID_KEY] = stage_id
    copied[_MINDMAP_SOURCE_ID_KEY] = source_id
    if read_only:
        copied[_MINDMAP_READ_ONLY_KEY] = True
    copied['children'] = []
    for child in node.get('children', []):
        copied_child = _copy_stage_mindmap_node(
            child,
            stage_id,
            id_map,
            read_only=read_only,
        )
        if copied_child is not None:
            copied['children'].append(copied_child)
    if is_root:
        copied[_MINDMAP_STAGE_ROOT_KEY] = True
    return copied


def _copy_stage_mindmap_item(item, stage_id, id_map, item_type):
    if not isinstance(item, dict):
        return None

    copied = _strip_mindmap_internal_keys(item)
    source_id = _valid_mindmap_id(item.get('id'), uuid.uuid4().hex)
    copied['id'] = _combined_stage_item_id(stage_id, source_id, item_type)
    copied[_MINDMAP_STAGE_ID_KEY] = stage_id
    copied[_MINDMAP_SOURCE_ID_KEY] = source_id

    if item_type == 'arrow':
        source_from = item.get('from')
        source_to = item.get('to')
        if source_from not in id_map or source_to not in id_map:
            return None
        copied['from'] = id_map[source_from]
        copied['to'] = id_map[source_to]
    else:
        source_parent = item.get('parent')
        if source_parent not in id_map:
            return None
        copied['parent'] = id_map[source_parent]
    return copied


def compose_project_mindmap(project):
    """Build a transient project map containing live branches for its stages."""
    project_map = normalize_mindmap_data(
        getattr(project, 'mindmap_data', None)
    )
    if project_map is None:
        project_map = _new_mindmap_data(
            project.name,
            f'project-map-{uuid.uuid4().hex}',
        )
    else:
        project_map['nodeData'] = _copy_project_mindmap_node(
            project_map['nodeData']
        )
        project_map['arrows'] = [
            _strip_mindmap_internal_keys(item)
            for item in project_map.get('arrows', [])
            if (
                isinstance(item, dict)
                and not item.get(_MINDMAP_STAGE_ID_KEY)
            )
        ]
        project_map['summaries'] = [
            _strip_mindmap_internal_keys(item)
            for item in project_map.get('summaries', [])
            if (
                isinstance(item, dict)
                and not item.get(_MINDMAP_STAGE_ID_KEY)
            )
        ]

    project_map.setdefault('arrows', [])
    project_map.setdefault('summaries', [])
    project_children = project_map['nodeData'].setdefault('children', [])

    for stage in getattr(project, 'stages', []):
        stage_id = _valid_mindmap_id(
            getattr(stage, 'stage_id', None),
            uuid.uuid4().hex,
        )
        stage_map = normalize_mindmap_data(
            getattr(stage, 'mindmap_data', None)
        )
        stage_map_has_content = mindmap_has_content(stage_map, stage.name)
        if stage_map is None:
            stage_map = _new_mindmap_data(
                stage.name,
                f'stage-map-{stage_id}',
            )

        id_map = {}
        stage_is_completed = stage.status == 'завершен'
        stage_root = _copy_stage_mindmap_node(
            stage_map['nodeData'],
            stage_id,
            id_map,
            is_root=True,
            read_only=stage_is_completed,
        )
        stage_root['topic'] = (
            f'✅ {stage.name}' if stage_is_completed else stage.name
        )
        if stage_is_completed and not stage_map_has_content:
            stage_root[_MINDMAP_EMPTY_STAGE_KEY] = True
        project_children.append(stage_root)

        for arrow in stage_map.get('arrows', []):
            copied_arrow = _copy_stage_mindmap_item(
                arrow,
                stage_id,
                id_map,
                'arrow',
            )
            if copied_arrow is not None:
                project_map['arrows'].append(copied_arrow)
        for summary in stage_map.get('summaries', []):
            copied_summary = _copy_stage_mindmap_item(
                summary,
                stage_id,
                id_map,
                'summary',
            )
            if copied_summary is not None:
                project_map['summaries'].append(copied_summary)

    return normalize_mindmap_data(project_map)


def _collect_combined_stage_roots(node, stage_roots):
    if not isinstance(node, dict):
        return
    if node.get(_MINDMAP_STAGE_ROOT_KEY):
        stage_id = node.get(_MINDMAP_STAGE_ID_KEY)
        if isinstance(stage_id, str) and stage_id:
            stage_roots.setdefault(stage_id, node)
    for child in node.get('children', []):
        _collect_combined_stage_roots(child, stage_roots)


def _restore_stage_mindmap_node(
        node,
        stage_id,
        source_ids,
        future_ids,
        owners,
        is_root=False,
):
    if not isinstance(node, dict):
        return None
    if not is_root and node.get(_MINDMAP_STAGE_ROOT_KEY):
        return None

    current_id = _valid_mindmap_id(node.get('id'), uuid.uuid4().hex)
    source_id = _valid_mindmap_id(
        node.get(_MINDMAP_SOURCE_ID_KEY),
        current_id,
    )
    source_ids[current_id] = source_id
    future_ids[current_id] = _combined_stage_node_id(
        stage_id,
        source_id,
        is_root,
    )
    owners[current_id] = stage_id

    restored = _strip_mindmap_internal_keys(node)
    restored.pop('parent', None)
    restored['id'] = source_id
    restored['children'] = []
    for child in node.get('children', []):
        restored_child = _restore_stage_mindmap_node(
            child,
            stage_id,
            source_ids,
            future_ids,
            owners,
        )
        if restored_child is not None:
            restored['children'].append(restored_child)
    return restored


def _restore_stage_mindmap_item(item):
    restored = _strip_mindmap_internal_keys(item)
    restored['id'] = _valid_mindmap_id(
        item.get(_MINDMAP_SOURCE_ID_KEY),
        _valid_mindmap_id(item.get('id'), uuid.uuid4().hex),
    )
    return restored


def split_combined_project_mindmap(project, combined_data):
    """Split a transient combined map into project and per-stage maps."""
    combined_map = normalize_mindmap_data(combined_data)
    if combined_map is None:
        raise ValueError('Invalid combined mind map data')

    stage_roots = {}
    _collect_combined_stage_roots(combined_map['nodeData'], stage_roots)
    project_root = _copy_project_mindmap_node(combined_map['nodeData'])
    if project_root is None:
        raise ValueError('Combined mind map has no project root')

    stage_maps = {}
    source_ids_by_stage = {}
    future_ids = {}
    node_owners = {}
    for stage in getattr(project, 'stages', []):
        stage_id = getattr(stage, 'stage_id', None)
        stage_root = stage_roots.get(stage_id)
        if stage_root is None:
            continue

        existing_map = normalize_mindmap_data(
            getattr(stage, 'mindmap_data', None)
        )
        if existing_map is None:
            existing_map = _new_mindmap_data(
                stage.name,
                f'stage-map-{stage_id}',
            )

        source_ids = {}
        restored_root = _restore_stage_mindmap_node(
            stage_root,
            stage_id,
            source_ids,
            future_ids,
            node_owners,
            is_root=True,
        )
        original_root = existing_map['nodeData']
        restored_root['id'] = original_root['id']
        restored_root['topic'] = original_root.get('topic', stage.name)
        if 'root' in original_root:
            restored_root['root'] = original_root['root']
        source_ids[stage_root['id']] = restored_root['id']
        future_ids[stage_root['id']] = _combined_stage_node_id(
            stage_id,
            restored_root['id'],
            is_root=True,
        )

        restored_map = deepcopy(existing_map)
        restored_map['nodeData'] = restored_root
        restored_map['arrows'] = []
        restored_map['summaries'] = []
        stage_maps[stage_id] = restored_map
        source_ids_by_stage[stage_id] = source_ids

    project_arrows = []
    for arrow in combined_map.get('arrows', []):
        if not isinstance(arrow, dict):
            continue
        from_id = arrow.get('from')
        to_id = arrow.get('to')
        from_owner = node_owners.get(from_id)
        to_owner = node_owners.get(to_id)
        if from_owner and from_owner == to_owner and from_owner in stage_maps:
            source_ids = source_ids_by_stage[from_owner]
            if from_id not in source_ids or to_id not in source_ids:
                continue
            restored_arrow = _restore_stage_mindmap_item(arrow)
            restored_arrow['from'] = source_ids[from_id]
            restored_arrow['to'] = source_ids[to_id]
            stage_maps[from_owner]['arrows'].append(restored_arrow)
            continue

        project_arrow = _strip_mindmap_internal_keys(arrow)
        if from_id in future_ids:
            project_arrow['from'] = future_ids[from_id]
        if to_id in future_ids:
            project_arrow['to'] = future_ids[to_id]
        project_arrows.append(project_arrow)

    project_summaries = []
    for summary in combined_map.get('summaries', []):
        if not isinstance(summary, dict):
            continue
        parent_id = summary.get('parent')
        owner = node_owners.get(parent_id)
        if owner and owner in stage_maps:
            source_ids = source_ids_by_stage[owner]
            if parent_id not in source_ids:
                continue
            restored_summary = _restore_stage_mindmap_item(summary)
            restored_summary['parent'] = source_ids[parent_id]
            stage_maps[owner]['summaries'].append(restored_summary)
            continue

        project_summary = _strip_mindmap_internal_keys(summary)
        if parent_id in future_ids:
            project_summary['parent'] = future_ids[parent_id]
        project_summaries.append(project_summary)

    project_map = deepcopy(combined_map)
    project_map['nodeData'] = project_root
    project_map['arrows'] = project_arrows
    project_map['summaries'] = project_summaries
    project_map = normalize_mindmap_data(project_map)
    stage_maps = {
        stage_id: normalize_mindmap_data(stage_map)
        for stage_id, stage_map in stage_maps.items()
    }
    return project_map, stage_maps


def now_for_test():
    """Возвращает текущие дату и время с учетом режима разработчика."""
    # Тестовая дата должна работать только в настоящем режиме разработчика.
    # Если этот флаг случайно попал в пользовательские данные, обычная сборка
    # не должна навсегда застревать на выбранном вручную дне.
    settings = load_settings()
    if dev_mode and settings.get('today_for_test_mode', False):
        selected_datetime = settings.get('today_for_test_datetime')
        if isinstance(selected_datetime, datetime):
            return selected_datetime

    return datetime.combine(date.today(), datetime.now().time())


def today_for_test():
    """Возвращает дату текущих суток с учетом выбранного времени их начала."""
    return writing_day_for_datetime(now_for_test())


def writing_day_for_datetime(value, settings=None):
    """Возвращает писательский день, которому принадлежит ``value``.

    Записи прогресса хранят дату именно этого дня, а не календарную дату
    момента записи. Это позволяет всем расчётам дневной цели, написанного за
    день и дедлайнов использовать одну и ту же границу суток.
    """
    if isinstance(value, datetime):
        current_time = value.time().replace(tzinfo=None)
        current_date = value.date()
    elif isinstance(value, date):
        return value
    else:
        raise TypeError('Ожидалась дата или дата и время.')

    start_day_time = get_start_day_time() if settings is None else get_start_day_time(settings)
    if current_time < start_day_time:
        return current_date - timedelta(days=1)
    return current_date


def writing_day_timestamp(value, settings=None):
    """Сохраняет время записи, заменяя календарную дату датой её писательского дня."""
    if not isinstance(value, datetime):
        raise TypeError('Ожидались дата и время.')
    return datetime.combine(
        writing_day_for_datetime(value, settings),
        value.time().replace(tzinfo=None),
    )


def get_start_day_time(settings=None):
    """Возвращает время начала суток из настроек.

    Значение хранится в формате ``HH:MM:SS``. Старые или поврежденные
    настройки безопасно используют полночь.
    """
    if settings is None:
        settings = load_settings()

    saved_time = settings.get('start_day_time', '00:00:00')
    if isinstance(saved_time, time):
        return saved_time.replace(tzinfo=None)
    if isinstance(saved_time, str):
        try:
            return time.fromisoformat(saved_time)
        except ValueError:
            pass
    return time.min


def is_developer_test_date_enabled():
    """Возвращает True, если дата вручную задана в режиме разработчика."""
    if not dev_mode:
        return False
    return load_settings().get('today_for_test_mode', False)


STREAK_FREEZE_MARKER = 'freeze'
STREAK_DATE_BUG_START = date(2026, 7, 9)
STREAK_DATE_BUG_LAST_SAFE_DAY = STREAK_DATE_BUG_START - timedelta(days=1)
STREAK_DATE_BUG_REPAIR_LAST_DAY = date(2026, 7, 10)
_project_freeze_batch = None


def is_streak_date(value):
    return isinstance(value, date) and not isinstance(value, datetime)


def iter_streak_days(streaks):
    """Возвращает пары (запись, эффективная дата) для дат и маркеров заморозки."""
    current_day = None
    if not isinstance(streaks, list):
        return

    for entry in streaks:
        if is_streak_date(entry):
            current_day = entry
            yield entry, current_day
        elif entry == STREAK_FREEZE_MARKER and current_day is not None:
            current_day = current_day + timedelta(days=1)
            yield entry, current_day


def streak_last_day(streaks):
    last_day = None
    for _, effective_day in iter_streak_days(streaks):
        last_day = effective_day
    return last_day


def streak_has_day_after(streaks, target_day):
    """Проверяет, есть ли в стрике день позднее даты пересчёта."""
    last_day = streak_last_day(streaks)
    return last_day is not None and last_day > target_day


def streak_bonus_is_due(last_bonus_day, today=None):
    """Возвращает True, если бонус за текущие сутки ещё не выдавался."""
    if today is None:
        today = today_for_test()
    if isinstance(last_bonus_day, datetime):
        last_bonus_day = last_bonus_day.date()
    return not isinstance(last_bonus_day, date) or last_bonus_day < today


def streak_contains_day(streaks, target_day):
    return any(effective_day == target_day for _, effective_day in iter_streak_days(streaks))


def streak_length(streaks):
    return sum(1 for _ in iter_streak_days(streaks))


def streak_is_freeze_for_day(streaks, target_day):
    return any(entry == STREAK_FREEZE_MARKER and effective_day == target_day
               for entry, effective_day in iter_streak_days(streaks))


def begin_project_freeze_batch(parent_project):
    """Группирует автозаморозки этапов одного проекта в один расход предмета."""
    global _project_freeze_batch
    previous_batch = _project_freeze_batch
    _project_freeze_batch = {
        'parent': parent_project,
        'spent_days': set(),
    }
    return previous_batch


def end_project_freeze_batch(previous_batch=None):
    global _project_freeze_batch
    _project_freeze_batch = previous_batch


def remove_streak_day(streaks, target_day):
    if not isinstance(streaks, list):
        return False

    current_day = None
    for index, entry in enumerate(streaks):
        if is_streak_date(entry):
            current_day = entry
        elif entry == STREAK_FREEZE_MARKER and current_day is not None:
            current_day = current_day + timedelta(days=1)
        else:
            continue

        if current_day == target_day:
            del streaks[index]
            return True
    return False


def repair_streak_gap_after_date_bug(streaks, target_day):
    """Заполняет разрыв, возникший из-за зависшей смены даты после 09.07.2026."""
    if is_developer_test_date_enabled():
        return False
    if not isinstance(streaks, list):
        return False

    last_day = streak_last_day(streaks)
    if not last_day or last_day < STREAK_DATE_BUG_LAST_SAFE_DAY:
        return False
    if target_day > STREAK_DATE_BUG_REPAIR_LAST_DAY:
        return False
    if last_day >= target_day:
        return False

    changed = False
    current_day = last_day + timedelta(days=1)
    while current_day <= target_day:
        if not streak_contains_day(streaks, current_day):
            streaks.append(current_day)
            changed = True
        current_day += timedelta(days=1)
    return changed


def rebuild_streak_ending_at(end_day, length):
    """Создает непрерывный стрик заданной длины с последним днем end_day."""
    if not isinstance(end_day, date) or length <= 0:
        return []

    start_day = end_day - timedelta(days=length - 1)
    return [start_day + timedelta(days=offset) for offset in range(length)]

def load_settings():
    """Загружает данные из кроссплатформенной директории"""
    data_file = get_data_file_path('settings')
    try:
        with open(data_file, 'rb') as f:
            return pickle.load(f)
    except (FileNotFoundError, EOFError):
        # Если файл не найден, создаём пустую структуру
        return {
            'game_mode': False,
            'inf_project': False,
            'global_streak': False,
            'show_written_today_in_all_projects': False,
            'notification_display_time': 10,
            'start_day_time': '00:00:00',
            'inventory_filter': 'Все',
        }

def save_settings(data):
    """Сохраняет данные в кроссплатформенную директорию"""
    data_file = get_data_file_path('settings')
    atomic_pickle_save(data, data_file)
    _sync_shadow_after_legacy_save(data_file)


def atomic_pickle_save(data, data_file):
    """Атомарно сохраняет pickle в файл через уникенный временный файл в той же директории."""
    data_file = Path(data_file)
    data_file.parent.mkdir(parents=True, exist_ok=True)

    fd, temp_path_str = tempfile.mkstemp(
        prefix=f'{data_file.stem}_',
        suffix='.tmp',
        dir=str(data_file.parent)
    )
    temp_path = Path(temp_path_str)

    try:
        with os.fdopen(fd, 'wb') as f:
            pickle.dump(data, f)
            f.flush()
            os.fsync(f.fileno())
        temp_path.replace(data_file)
    except Exception:
        if temp_path.exists():
            temp_path.unlink()
        raise


def _sync_shadow_after_legacy_save(data_file):
    """Refresh the derived SQLite mirror after an authoritative file write."""
    from nfprogress.core.legacy_shadow import sync_legacy_sqlite_shadow

    sync_legacy_sqlite_shadow(Path(data_file).parent)


def reconcile_legacy_sqlite_shadow():
    """Heal the bridge mirror from the active legacy profile at startup."""
    from nfprogress.core.legacy_shadow import reconcile_legacy_sqlite_shadow as reconcile

    return reconcile(Path(get_data_file_path('data')).parent)

class Project:
    def __init__(self, name='Без имени', goal=None,
                 create_date=None, total_symbols=0, progress=0,
                 notes=None, streaks=None, max_streak=None, streak_status='No', deadline='Нет',
                 status='активен', unit='symbols', personal_goal_for_the_day=0,
                 auto_freeze=True):

        self._name = name
        self.project_id = uuid.uuid4().hex
        self._goal = goal  # хранится в выбранной единице
        self.create_date = create_date if create_date else today_for_test()
        self.edit_date = create_date if create_date else today_for_test()
        self.complete_date = None
        self._total_symbols = total_symbols  # хранится в выбранной единице
        self._progress = progress
        self._deadline = deadline
        self._status = status
        self.notes = notes if notes else []
        self.streaks = streaks if streaks else []
        self.max_streak = max_streak if max_streak else 0
        self.streak_status = streak_status
        self.unit = unit
        self.synch = None
        self.last_synch = None
        self.work_method = 'manual'
        self.last_streak_bonus = None
        self.last_streak_lost_date = None
        self.freezes = 0
        self.auto_freeze = bool(auto_freeze)
        self.deadline_set_date = today_for_test()
        self.personal_goal_for_the_day = personal_goal_for_the_day
        self.project_plan = {}
        self.enable_stages = False
        self.stages = []
        self.is_stage = False
        self.mindmap_data = None
        self.mindmap_updated_at = None
        self.combine_stage_mindmaps = False
        self.project_notes = []
        self.notes_updated_at = None
        # A cropped 2:3 data URL used by the Vue project cards.  Keeping it on
        # the project makes covers available to desktop and offline clients.
        self.cover_image = None
        # Optional stable identifier of a user-created project folder.  The
        # folder catalogue itself lives beside the projects in the save file.
        self.folder_id = None

    def migrate(self):
        """Проверяет наличие всех атрибутов и добавляет недостающие"""
        # Удаляем промежуточный дублирующий флаг: состояние полностью хранится
        # в streak_status ('Off' — выключено, остальные статусы — включено).
        if hasattr(self, 'streak_enabled'):
            delattr(self, 'streak_enabled')
        # Old saves have no ``work_method``.  Remember that before adding
        # defaults so an existing external binding can be migrated to sync.
        has_work_method = hasattr(self, 'work_method')
        defaults = {
            '_name': 'Без имени',
            'project_id': uuid.uuid5(
                uuid.NAMESPACE_URL,
                f'nfprogress-project:{getattr(self, "_name", "Без имени")}',
            ).hex,
            '_goal': None,
            'create_date': today_for_test(),
            'edit_date': today_for_test(),
            'complete_date': None,
            '_total_symbols': 0,
            '_progress': 0,
            '_deadline': 'Нет',
            '_status': 'активен',
            'notes': [],
            'streaks': [],
            'max_streak': 0,
            'streak_status': 'No',
            'unit': 'symbols',
            'synch': None,
            'last_synch': None,
            'work_method': 'manual',
            'last_streak_bonus': None,
            'last_streak_lost_date': None,
            'freezes': 0,
            'auto_freeze': True,
            'deadline_set_date': today_for_test(),
            'project_plan': {},
            'enable_stages': False,
            'stages': [],
            'is_stage': False,
            'mindmap_data': None,
            'mindmap_updated_at': None,
            'combine_stage_mindmaps': False,
            'project_notes': [],
            'notes_updated_at': None,
            'cover_image': None,
            'folder_id': None,
        }

        for attr, default_value in defaults.items():
            if not hasattr(self, attr):
                setattr(self, attr, default_value)
            elif attr in ('notes', 'streaks', 'stages', 'project_notes') and not isinstance(getattr(self, attr), list):
                setattr(self, attr, [])

        if not isinstance(self.project_id, str) or not self.project_id:
            # ``load_data()`` intentionally does not save on a read.  A random
            # migration value would therefore change between two API requests.
            self.project_id = defaults['project_id']
        self._ensure_progress_entry_ids()
        self.project_notes = normalize_project_note_records(self.project_notes)

        # --- Вычисляем динамические данные ПОСЛЕ того, как базовые атрибуты созданы ---
        if not hasattr(self, 'personal_goal_for_the_day'):
            self.personal_goal_for_the_day = self.get_today_goal_in_unit()

        # Если если нет плана - генерируем его
        if self.project_plan == {}:
            self.get_today_goal_value()

        if self.synch is not None and isinstance(self.synch, str):
            self.synch = {'type': 'word', 'path': self.synch}
        if not has_work_method or getattr(self, 'work_method', None) not in {'manual', 'sync', 'app'}:
            # Existing external bindings remain active after migration; all
            # other legacy projects keep their established manual workflow.
            self.work_method = 'sync' if self.synch is not None else 'manual'

        # deadline_set_date должен храниться как date, иначе старт плана "прыгает".
        if self.deadline_set_date is None or not isinstance(self.deadline_set_date, date):
            self.deadline_set_date = today_for_test()

        if not isinstance(getattr(self, 'stages', []), list):
            self.stages = []

        if (
                self.mindmap_data is not None
                and not _mindmap_data_has_valid_root(self.mindmap_data)
        ):
            self.mindmap_data = None
        if not isinstance(
                getattr(self, 'mindmap_updated_at', None),
                (str, datetime, date),
        ):
            self.mindmap_updated_at = None
        if not isinstance(
                getattr(self, 'notes_updated_at', None),
                (str, datetime, date),
        ):
            self.notes_updated_at = None
        if not isinstance(self.combine_stage_mindmaps, bool):
            self.combine_stage_mindmaps = False
        self.cover_image = normalize_project_cover_image(self.cover_image)
        if not isinstance(self.folder_id, str) or not self.folder_id:
            self.folder_id = None

        for index, stage in enumerate(self.stages):
            if not isinstance(stage, Stage):
                converted_stage = Stage(
                    name=getattr(stage, 'name', f'Этап {index + 1}'),
                    goal=getattr(stage, 'goal', 0),
                    create_date=getattr(stage, 'create_date', self.create_date),
                    total_symbols=getattr(stage, 'total_units', 0),
                    progress=getattr(stage, 'progress', 0),
                    notes=getattr(stage, 'notes', []),
                    streaks=getattr(stage, 'streaks', []),
                    max_streak=getattr(stage, 'max_streak', 0),
                    streak_status=getattr(stage, 'streak_status', 'No'),
                    deadline=getattr(stage, 'deadline', 'Нет'),
                    status=getattr(stage, 'status', self.status),
                    unit=getattr(stage, 'unit', self.unit),
                    personal_goal_for_the_day=getattr(stage, 'personal_goal_for_the_day', 0),
                    auto_freeze=getattr(stage, 'auto_freeze', True),
                    parent_project_name=self.name,
                )
                converted_stage.synch = getattr(stage, 'synch', None)
                converted_stage.last_synch = getattr(stage, 'last_synch', None)
                converted_stage.mindmap_data = normalize_mindmap_data(
                    getattr(stage, 'mindmap_data', None)
                )
                converted_stage.mindmap_updated_at = getattr(
                    stage, 'mindmap_updated_at', None,
                )
                converted_stage.notes_updated_at = getattr(
                    stage, 'notes_updated_at', None,
                )
                converted_stage.project_notes = normalize_project_note_records(
                    getattr(stage, 'project_notes', [])
                )
                self.stages[index] = converted_stage
                stage = converted_stage
            had_stable_stage_id = bool(
                isinstance(getattr(stage, 'stage_id', None), str)
                and stage.stage_id
            )
            stage.parent_project_name = self.name
            stage.migrate()
            if not had_stable_stage_id:
                stage.stage_id = uuid.uuid5(
                    uuid.NAMESPACE_URL,
                    (
                        f'nfprogress-stage:{self.project_id}:{index}:'
                        f'{stage.name}:{getattr(stage, "create_date", None)}'
                    ),
                ).hex
        if not self.has_stages():
            self.combine_stage_mindmaps = False

    def _ensure_progress_entry_ids(self):
        """Give legacy progress rows stable API identifiers without changing values."""
        for index, note in enumerate(self.notes):
            entry_id = getattr(note, 'entry_id', None)
            if isinstance(entry_id, str) and entry_id:
                continue
            created_at = getattr(note, 'date_create', None)
            if isinstance(created_at, datetime):
                created_key = created_at.isoformat()
            else:
                created_key = repr(created_at)
            seed = (
                f'nfprogress-progress:{self.project_id}:{created_key}:'
                f'{getattr(note, "new_total", None)}:'
                f'{getattr(note, "added_symbols", None)}:{index}'
            )
            note.entry_id = uuid.uuid5(uuid.NAMESPACE_URL, seed).hex

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, name):
        if hasattr(self, '_name') and self._name == name:
            self._name = name
            return

        data = load_data()
        projects = data['projects']
        names = [i for i in projects.keys() if i != self]
        if name != '':
            if name in names:
                raise ValueError('Проект с таким именем уже существует!')
            else:
                self._name = name

    @property
    def goal(self):
        if self.has_stages():
            goal_symbols = 0
            for stage in self.stages:
                stage_goal = stage.get_goal_symbols()
                if stage_goal == float('inf'):
                    return float('inf')
                goal_symbols += stage_goal
            return unit_converter('symbols', goal_symbols, self.unit)
        return self._goal

    @goal.setter
    def goal(self, goal):
        try:
            self._goal = float(goal)
        except ValueError:
            raise ValueError('Цель должна быть числом!')

    @property
    def deadline(self):
        if self._deadline != 'Нет':
            return self._deadline
        return 'Нет'

    @deadline.setter
    def deadline(self, deadline):
        if deadline in ('', 'Нет', None):
            self._deadline = 'Нет'
            self.deadline_set_date = None
        else:
            self._deadline = deadline
            self.deadline_set_date = today_for_test()

    def set_streak_state(self, enabled):
        """Включает или отключает локальный стрик независимо от дедлайна."""
        enabled = bool(enabled)
        if (self.streak_status != 'Off') == enabled:
            return

        if self.streaks:
            self.max_streak = max(self.max_streak, streak_length(self.streaks))
        self.streaks = []
        self.last_streak_bonus = None
        self.last_streak_lost_date = None
        self.streak_status = 'No' if enabled else 'Off'

    @property
    def deadline_str(self):
        if self._deadline != 'Нет':
            return datetime.strftime(self.deadline, '%d.%m.%y')
        return 'Нет'

    @property
    def status(self):
        return self._status

    @status.setter
    def status(self, status):
        self._status = status

    @property
    def total_units(self):
        if self.has_stages():
            total_symbols = sum(stage.get_total_symbols() for stage in self.stages)
            return unit_converter('symbols', total_symbols, self.unit)
        return self._total_symbols

    @total_units.setter
    def total_units(self, total_symbols):
        self._total_symbols = total_symbols

    def has_stages(self):
        """Возвращает True, если проект работает как контейнер этапов."""
        return bool(getattr(self, 'enable_stages', False) and getattr(self, 'stages', []))

    def get_goal_symbols(self):
        """Возвращает цель в символах."""
        if self.has_stages():
            total = 0
            for stage in self.stages:
                stage_goal = stage.get_goal_symbols()
                if stage_goal == float('inf'):
                    return float('inf')
                total += stage_goal
            return total
        if self._goal == float('inf'):
            return float('inf')
        return unit_converter(self.unit, self._goal, 'symbols')

    def get_total_symbols(self):
        """Возвращает текущее количество в символах."""
        if self.has_stages():
            return sum(stage.get_total_symbols() for stage in self.stages)
        return unit_converter(self.unit, self._total_symbols, 'symbols')

    def was_goal_completed_by_deadline(self):
        """Возвращает, была ли цель достигнута не позднее даты дедлайна."""
        if not isinstance(self.deadline, date):
            return False

        goal_symbols = self.get_goal_symbols()
        if goal_symbols == float('inf'):
            return False

        if self.has_stages():
            total_by_deadline = sum(
                stage._get_total_symbols_by_date(self.deadline)
                for stage in self.stages
            )
        else:
            total_by_deadline = self._get_total_symbols_by_date(self.deadline)
        return total_by_deadline >= goal_symbols

    def _get_total_symbols_by_date(self, target_date):
        """Возвращает последнее зафиксированное количество символов на указанную дату."""
        notes = [
            note for note in self.notes
            if self._get_note_progress_date(note) <= target_date
        ]
        if not notes:
            return 0
        latest_note = max(notes, key=lambda note: note.date_create)
        return latest_note.get_new_total()

    @staticmethod
    def _get_note_progress_date(note):
        """Возвращает дату записи с учётом настроенного начала писательских суток."""
        # ``Note`` already stores its date as the writing-day date. Applying
        # the start-of-day offset here a second time moved entries created
        # before the boundary back by two calendar days.
        return note.get_date_create()

    @property
    def progress(self):
        """Прогресс проекта в процентах"""
        goal_sym = self.get_goal_symbols()
        total_sym = self.get_total_symbols()
        if goal_sym and goal_sym > 0 and goal_sym != float('inf'):
            self._progress = total_sym / goal_sym * 100
        else:
            self._progress = 0
        return self._progress

    def get_added_symbols_today_value(self):
        """Возвращает количество добавленных сегодня символов."""
        if self.has_stages():
            return sum(stage.get_added_symbols_today_value() for stage in self.stages)
        today = today_for_test()
        today_added = [i.get_added_symbols() for i in self.notes if i.get_date_create() == today]
        return sum(today_added) if today_added else 0

    def get_added_today_in_unit(self):
        """Возвращает количество добавленных сегодня в единице проекта."""
        added_sym = self.get_added_symbols_today_value()
        return unit_converter('symbols', added_sym, self.unit)

    def get_today_goal_value(
            self,
            recalculate_from_current_progress: bool = False,
    ):
        """
        Возвращает накопительный план на сегодня в символах.
        Генерирует план от текущей даты до дедлайна по принципу "лесенки".

        При явном пересчёте сегодняшняя ступень начинается от текущего
        прогресса, а не от прогресса на начало писательского дня.
        """
        today = today_for_test()

        if self.has_stages() and self.deadline == 'Нет':
            stage_daily_goals = [
                stage.get_today_goal_value()
                for stage in self.stages
                if (
                    stage.status == 'активен'
                    and (
                        stage.deadline != 'Нет'
                        or getattr(stage, 'personal_goal_for_the_day', 0)
                    )
                )
            ]
            if stage_daily_goals:
                return sum(stage_daily_goals)
            if not getattr(self, 'personal_goal_for_the_day', 0):
                return 0

        if getattr(self, 'project_plan', None) is None:
            self.project_plan = {}

        plan = self.project_plan

        # === 1. ПРОВЕРКА ДЕДЛАЙНА (Если он наступил или прошел) ===
        if self.deadline != 'Нет' and isinstance(self.deadline, date):
            if today >= self.deadline:
                return self.get_goal_symbols()

        # === 2. БЫСТРЫЙ ВОЗВРАТ ИЗ КЭША (Защита от "убегания" цели) ===
        # Создаем "слепок" параметров, от которых зависит план
        stages_signature = tuple(
            (
                getattr(stage, 'stage_id', None),
                stage.get_goal_symbols(),
                getattr(stage, 'deadline', 'Нет'),
                getattr(stage, 'personal_goal_for_the_day', 0),
            )
            for stage in self.stages
        ) if self.has_stages() else ()
        current_signature = str((
            getattr(self, 'deadline', 'Нет'),
            getattr(self, 'personal_goal_for_the_day', 0),
            self.get_goal_symbols(),
            getattr(self, 'unit', 'symbols'),
            getattr(self, 'deadline_set_date', None),
            stages_signature,
        ))

        # Если день есть в плане И настройки не менялись с момента последнего расчета — отдаем кэш
        if (
                not recalculate_from_current_progress
                and today in plan
                and plan.get('signature') == current_signature
        ):
            return plan[today]

        # === 3. ПЕРЕСЧЕТ ПЛАНА (Инициализация) ===

        previous_today_goal = None if recalculate_from_current_progress else plan.get(today)
        previous_daily_goal = (
            None if recalculate_from_current_progress else plan.get('daily_goal_symbols')
        )
        if previous_daily_goal is None:
            next_day_goal = plan.get(today + timedelta(days=1))
            if isinstance(previous_today_goal, (int, float)) and isinstance(next_day_goal, (int, float)):
                previous_daily_goal = next_day_goal - previous_today_goal
        previous_past_goals = {
            plan_date: goal
            for plan_date, goal in plan.items()
            if isinstance(plan_date, date) and plan_date < today
        }

        # При изменении цели или дедлайна план строится заново. Базой для
        # сегодняшней цели должен быть прогресс на начало дня, иначе уже
        # написанное сегодня попадёт в базу и новая дневная норма прибавится
        # к нему повторно.
        today_added_symbols = self.get_added_symbols_today_value()
        if not today_added_symbols and hasattr(self, 'streaks') and isinstance(self.streaks, list):
            # Поддерживаем старые сохранения, в которых сегодняшнее значение
            # могло храниться только в записи стрика.
            for streak in self.streaks:
                if isinstance(streak, dict) and streak.get('date') == today:
                    today_added_symbols = streak.get('count', 0)
                    break

        base_total = unit_converter(self.unit, self.total_units)
        if not recalculate_from_current_progress:
            base_total -= today_added_symbols
        daily_goal_symbols = 0

        if getattr(self, 'personal_goal_for_the_day', 0):
            converted = unit_converter(self.unit, self.personal_goal_for_the_day, 'symbols')
            if converted == float('inf'):
                daily_goal_symbols = float('inf')
            else:
                # Убираем ceil – оставляем дробное значение
                daily_goal_symbols = converted

        elif self.deadline != 'Нет' and isinstance(self.deadline, date):

            if self.goal != float('inf'):
                goal_sym = unit_converter(self.unit, self.goal)
                total_days = (self.deadline - today).days + 1
                if total_days > 0:
                    remaining = max(0, goal_sym - base_total)
                    # Деление без округления вверх
                    daily_goal_symbols = remaining / total_days
                else:
                    daily_goal_symbols = goal_sym

        preserve_today_goal = (
            isinstance(previous_today_goal, (int, float))
            and isinstance(previous_daily_goal, (int, float))
            and math.isclose(previous_daily_goal, daily_goal_symbols)
        )
        adjust_today_goal_by_daily_difference = (
            isinstance(previous_today_goal, (int, float))
            and isinstance(previous_daily_goal, (int, float))
            and not math.isclose(previous_daily_goal, daily_goal_symbols)
        )

        # === 4. ГЕНЕРАЦИЯ ЛЕСЕНКИ ПЛАНА ===

        # Если кэш устарел (настройки поменялись) или его нет — строим заново
        if (
                recalculate_from_current_progress
                or not plan
                or plan.get('signature') != current_signature
        ):
            plan.clear()
            plan['signature'] = current_signature  # Сохраняем слепок настроек
            plan['daily_goal_symbols'] = daily_goal_symbols
            plan.update(previous_past_goals)

            end_date = self.deadline if (self.deadline != 'Нет' and isinstance(self.deadline, date)) else (
                        today + timedelta(days=30))

            current_date = today
            current_goal = base_total

            if preserve_today_goal:
                current_goal = previous_today_goal
                if self.deadline != 'Нет' and isinstance(self.deadline, date):
                    current_goal = min(current_goal, self.get_goal_symbols())
                plan[current_date] = current_goal
                current_date += timedelta(days=1)
            elif adjust_today_goal_by_daily_difference:
                current_goal = previous_today_goal - previous_daily_goal

            while current_date <= end_date:
                current_goal += daily_goal_symbols
                if self.deadline != 'Нет' and isinstance(self.deadline, date):
                    max_sym = self.get_goal_symbols()
                    if current_goal > max_sym:
                        current_goal = max_sym

                plan[current_date] = current_goal
                current_date += timedelta(days=1)

        else:
            # Сценарий достройки плана (если закончились 30 дней запаса)
            # Отфильтровываем строковые ключи (наш signature), оставляем только даты
            date_keys = [k for k in plan.keys() if isinstance(k, date)]
            if not date_keys:
                last_date = today
                current_goal = base_total
            else:
                last_date = max(date_keys)
                current_goal = plan[last_date]

            current_date = last_date + timedelta(days=1)
            end_date = today + timedelta(days=30)

            while current_date <= end_date:
                current_goal += daily_goal_symbols
                if self.deadline != 'Нет' and isinstance(self.deadline, date):
                    max_sym = self.get_goal_symbols()
                    if current_goal > max_sym:
                        current_goal = max_sym
                plan[current_date] = current_goal
                current_date += timedelta(days=1)

        return plan.get(today, base_total + daily_goal_symbols)

    def get_today_goal_in_unit(self):
        """Возвращает цель на сегодня в единице проекта."""
        goal_sym = self.get_today_goal_value()
        if goal_sym == float('inf'):
            return float('inf')
        return unit_converter('symbols', goal_sym, self.unit)

    def get_today_display_goal_value(self):
        """Возвращает накопительную цель для отображения родительского проекта."""
        if self.has_stages() and self.deadline == 'Нет':
            # The parent is a cumulative manuscript view. Even an этап without
            # its own deadline or daily quota contributes its current volume to
            # that total; otherwise the displayed target incorrectly collapses
            # to the one stage that happens to have a daily plan.
            visible_stages = [
                stage
                for stage in self.stages
                if stage.status != 'в архиве'
            ]
            if visible_stages:
                return sum(stage.get_today_goal_value() for stage in visible_stages)
            if not getattr(self, 'personal_goal_for_the_day', 0):
                return 0
        return self.get_today_goal_value()

    def get_today_display_goal_in_unit(self):
        """Возвращает накопительную цель для отображения в единице проекта."""
        goal_sym = self.get_today_display_goal_value()
        if goal_sym == float('inf'):
            return float('inf')
        return unit_converter('symbols', goal_sym, self.unit)

    def get_need_write_value(self):
        """Возвращает остаток написать в символах."""
        goal_sym = self.get_goal_symbols()
        total_sym = self.get_total_symbols()
        if goal_sym == float('inf'):
            return float('inf')
        return max(0, goal_sym - total_sym)

    def get_need_write_in_unit(self):
        """Возвращает остаток написать в единице проекта."""
        need_sym = self.get_need_write_value()
        if need_sym == float('inf'):
            return float('inf')
        return unit_converter('symbols', need_sym, self.unit)

    def get_streak_status(self, *, force=False):
        """Возвращает статус локального стрика проекта (статусы потери только в день потери)."""
        if self.streak_status == 'Off':
            return 'Off'

        today = today_for_test()
        yesterday = today - timedelta(days=1)
        total = self.get_total_symbols()
        planned = self.get_today_goal_value()

        # Завершенный проект или этап всегда завершает локальный стрик.
        if self.status == 'завершен':
            return 'Complete'

        # Если стрик сегодня уже продлен - не проверяем
        # Добавлена проверка на наличие элементов в self.streaks перед обращением к [-1]
        last_streak_day = streak_last_day(self.streaks)
        if self.streak_status == 'Go' and last_streak_day == today:
            return 'Go'

        # Заморозка
        if self.streak_status == 'Freeze' and streak_is_freeze_for_day(self.streaks, today):
            return 'Freeze'

        # Если проект бесконечный - стрика нет
        if planned == 0 or self.goal == float('inf'):
            return 'No'

        # Убедимся, что streaks — список
        if not isinstance(self.streaks, list):
            self.streaks = []

        # После изменения времени начала суток эффективная дата временно может
        # стать меньше даты уже сохранённого стрика. Это не пропуск дня, и
        # автоматическая проверка не должна сбрасывать стрик.
        if not force and streak_has_day_after(self.streaks, today):
            return self.streak_status

        day_completed = total >= planned

        if (not self.streaks and
                isinstance(self.streak_status, str) and
                self.streak_status.startswith('Lose ') and
                self.last_streak_lost_date is not None and
                self.last_streak_lost_date >= STREAK_DATE_BUG_START):
            status_parts = self.streak_status.split()
            if len(status_parts) >= 2 and status_parts[1].isdigit():
                lost_len = int(status_parts[1])
                self.streaks = rebuild_streak_ending_at(STREAK_DATE_BUG_LAST_SAFE_DAY, lost_len)
                self.streak_status = 'Active'
                self.last_streak_lost_date = None

        # === СБРОС УСТАРЕВШЕГО СТАТУСА ПОТЕРИ (чистого) ===
        if (isinstance(self.streak_status, str) and
                self.streak_status.startswith('Lose ') and
                len(self.streak_status.split()) == 2 and
                self.last_streak_lost_date is not None and
                self.last_streak_lost_date < today):
            # Прошёл день потери без активности — сбрасываем
            self.streak_status = 'No'
            self.last_streak_lost_date = None

        # === НОРМАЛИЗАЦИЯ КОМБИНИРОВАННОГО СТАТУСА (только для сегодня) ===
        if (isinstance(self.streak_status, str) and
                self.streak_status.startswith('Lose ') and
                len(self.streak_status.split()) == 3):
            # Если это не сегодняшний стрик, превращаем в обычный Start
            if streak_last_day(self.streaks) != today:
                self.streak_status = 'Start'
            else:
                parts = self.streak_status.split()
                self.streak_status = parts[2]  # сохраняем базовый статус ('Start')
                return f'Lose {parts[1]} {parts[2]}'  # возвращаем комбо для сообщения

        # === ПЕРСИСТЕНЦИЯ СТАТУСА ПОТЕРИ (только в день потери) ===
        if (isinstance(self.streak_status, str) and
                self.streak_status.startswith('Lose ') and
                len(self.streak_status.split()) == 2 and
                self.last_streak_lost_date == today and
                not day_completed):
            return self.streak_status

        # === ❗️ РЕТРОСПЕКТИВНАЯ ПРОВЕРКА ПРОПУЩЕННЫХ ДНЕЙ ===
        # Проверяем и "дописываем" прошлые дни ДО обработки сегодняшнего результата
        last_streak_day = streak_last_day(self.streaks)
        repair_streak_gap_after_date_bug(self.streaks, yesterday)
        last_streak_day = streak_last_day(self.streaks)
        if last_streak_day and last_streak_day < yesterday:
            current_day = last_streak_day + timedelta(days=1)
            plan = self.project_plan

            # Проверяем дни вплоть до вчерашнего
            while current_day < today:
                # Пропущенный день нельзя закрывать текущим накопительным total:
                # иначе написанное позже ретроактивно продлевает стрик без заморозки.
                if not apply_project_freeze(self, current_day, require_auto_freeze=True):
                    break
                current_day += timedelta(days=1)

        # Случай 1: сегодня уже есть запись в streaks (уже продлили сегодня)
        last_streak_day = streak_last_day(self.streaks)
        if last_streak_day == today and day_completed:
            self.streak_status = 'Go' if streak_length(self.streaks) > 1 else 'Start'
            self.last_streak_lost_date = None  # стрик активен, потери нет
            return self.streak_status

        # Случай 2: день выполнен
        if day_completed:
            if not self.streaks:
                # Первый день стрика
                self.streaks = [today]
                self.streak_status = 'Start'
                self.last_streak_lost_date = None
            elif last_streak_day == yesterday:
                # Продолжение стрика (в том числе если мы его восстановили шагом выше)
                self.streaks.append(today)
                self.streak_status = 'Go'
                self.last_streak_lost_date = None
            else:
                # Перерыв: потеря предыдущего + начало нового
                # (Сюда мы попадем, если ретроспективная проверка прервалась из-за нехватки символов)
                lose_len = streak_length(self.streaks)
                if lose_len > self.max_streak:
                    self.max_streak = lose_len
                self.streaks = [today]
                # Комбинированный статус (потеря + старт)
                self.streak_status = f'Lose {lose_len} Start'
                self.last_streak_lost_date = None
                return self.streak_status
        else:
            # День не выполнен
            if not self.streaks:
                self.streak_status = 'No'
            elif last_streak_day == today:
                # Уже сегодня была запись, но план не выполнен
                self.streak_status = 'Active'
            elif last_streak_day == yesterday:
                # Вчера был стрик, сегодня ещё не выполнен → активный
                self.streak_status = 'Active'
            else:
                # Потеря стрика (пропуск >1 дня и недобор символов)
                lose_len = streak_length(self.streaks)
                if lose_len > self.max_streak:
                    self.max_streak = lose_len
                self.streaks = []
                self.streak_status = f'Lose {lose_len}'
                self.last_streak_lost_date = today

        # Обновляем максимальный стрик, если текущая длина больше
        current_streak_len = streak_length(self.streaks)
        if current_streak_len > self.max_streak:
            self.max_streak = current_streak_len

        return self.streak_status

    def get_streak_status_msg(self, msg_type=None):
        """Возвращает сообщение о статусе стрика."""
        status = self.get_streak_status()
        if status == 'Start':
            if msg_type == 'min':
                return '🔥 стрик Начат'
            return f'🔥 Стрик в {self.name} начат! Отличное начало, главное - продолжать!'
        elif status == 'Freeze':
            if msg_type == 'min':
                return f'❄️ Стрик заморожен'
            return f'❄️ Стрик в {self.name} заморожен'
        elif status == 'Go':
            streaks = streak_length(self.streaks)
            if msg_type == 'min':
                return '🚀 стрик продлен'
            return f'🚀 Стрик в {self.name} продлен! Вы движетесь к цели уже {streaks} дней подряд!'
        elif status == 'Active':
            streaks = streak_length(self.streaks)
            if msg_type == 'min':
                return f'⏳ Стрик в {streaks} дн не продлен'
            return f'⏳ Стрик в {self.name} активен ({streaks} дн.), но ещё не продлён сегодня'
        elif status == 'Complete':
            streaks = streak_length(self.streaks)
            if msg_type == 'min':
                return '🎉 СТРИК ЗАВЕРШЕН'
            return f'🎉 СТРИК ЗАВЕРШЕН! Вы выполняли цель {streaks} дней подряд, потрясающе!'
        elif status == 'No':
            return f'🙃 Стрик не начат'
        elif status == 'Off':
            return 'Стрики выключены'
        elif status.split()[0] == 'Lose':
            status_parts = status.split()
            if len(status_parts) == 2:
                if msg_type == 'min':
                    return f'💔 Потеряно {status_parts[1]} д. стрика'
                return f'💔 Стрик потерян! Вы были в цели {status_parts[1]} дней подряд.'
            elif len(status_parts) == 3:
                return (f'💔 Стрик потерян! Вы были в цели {status_parts[1]} дней подряд.'
                        f'\n🔥 Вы начали новый стрик!')
        return 'Вывод статуса не работает'

    def set_new_notes(self, new_note):
        """Добавляет заметку и обновляет total_units в единице проекта."""
        self.notes.append(new_note)
        # new_note.new_total хранится в символах, конвертируем в единицу проекта
        self._total_symbols = unit_converter('symbols', new_note.new_total, self.unit)

    def get_notes_with_stage_names(self):
        """Возвращает записи проекта: для этапного проекта вместе с названием этапа."""
        if not self.has_stages():
            return [(None, note) for note in self.notes]

        notes = []
        for stage in self.stages:
            for note in stage.notes:
                notes.append((stage.name, note))
        return sorted(notes, key=lambda item: item[1].date_create)

    def get_latest_note(self):
        """Возвращает последнюю запись проекта с учетом этапов."""
        notes = self.get_notes_with_stage_names()
        if not notes:
            return None, None
        return max(notes, key=lambda item: item[1].date_create)

    def convert_to_stages(self):
        """Преобразует обычный проект в проект с одним первым этапом."""
        if self.has_stages():
            return

        stage = Stage(
            name='Этап 1',
            goal=self._goal,
            create_date=self.create_date,
            total_symbols=self._total_symbols,
            progress=self._progress,
            notes=list(self.notes),
            streaks=list(self.streaks),
            max_streak=self.max_streak,
            streak_status=self.streak_status,
            deadline=self._deadline,
            status=self.status,
            unit=self.unit,
            personal_goal_for_the_day=self.personal_goal_for_the_day,
            auto_freeze=self.auto_freeze,
            parent_project_name=self.name,
        )
        stage.synch = self.synch
        stage.last_synch = self.last_synch
        stage.work_method = getattr(
            self,
            'work_method',
            'sync' if self.synch is not None else 'manual',
        )
        stage.last_streak_bonus = None
        stage.last_streak_lost_date = None
        stage.freezes = 0
        stage.deadline_set_date = self.deadline_set_date
        stage.project_plan = dict(self.project_plan) if isinstance(self.project_plan, dict) else {}

        self.enable_stages = True
        self.stages = [stage]
        self.notes = []
        self.synch = None
        self.last_synch = None
        self._deadline = 'Нет'
        self.deadline_set_date = None
        self.streaks = []
        self.streak_status = 'Off'
        self.last_streak_bonus = None
        self.last_streak_lost_date = None

    def convert_to_single(self):
        """Преобразует проект с этапами в обычный проект с общей хронологией записей."""
        if not self.has_stages():
            self.enable_stages = False
            self.stages = []
            self.combine_stage_mindmaps = False
            return

        stage_with_longest_streak = self.get_stage_with_longest_streak()
        has_longest_stage_streak = (
                stage_with_longest_streak is not None
                and streak_length(getattr(stage_with_longest_streak, 'streaks', [])) > 0
        )
        self.transfer_longest_stage_streak(stage_with_longest_streak)

        goal_symbols = self.get_goal_symbols()
        total_symbols = self.get_total_symbols()
        self._goal = (
            float('inf')
            if goal_symbols == float('inf')
            else unit_converter('symbols', goal_symbols, self.unit)
        )

        merged = []
        stage_notes = self.get_notes_with_stage_names()
        # A stage may start with a non-zero value without a progress record.
        # Keep that baseline so conversion cannot silently reset the total.
        running_total_symbols = total_symbols - sum(
            note.get_added_symbols() for _, note in stage_notes
        )
        for _, note in stage_notes:
            added_symbols = note.get_added_symbols()
            running_total_symbols += added_symbols
            if goal_symbols == float('inf') or goal_symbols <= 0:
                added_progress = 0
            else:
                added_progress = added_symbols / goal_symbols * 100
            merged.append(Note(
                running_total_symbols,
                added_symbols,
                added_progress,
                note.date_create,
                entry_id=getattr(note, 'entry_id', None),
                writing_day=note.get_date_create(),
            ))

        self.notes = merged
        self._total_symbols = unit_converter('symbols', total_symbols, self.unit)
        self.synch = next((stage.synch for stage in self.stages if stage.synch is not None), None)
        stage_last_synchs = [stage.last_synch for stage in self.stages if stage.last_synch is not None]
        self.last_synch = max(stage_last_synchs) if stage_last_synchs else None
        if has_longest_stage_streak:
            self._deadline = getattr(stage_with_longest_streak, 'deadline', 'Нет')
            self.deadline_set_date = getattr(stage_with_longest_streak, 'deadline_set_date', None)
            self.auto_freeze = getattr(stage_with_longest_streak, 'auto_freeze', True)
        self.project_plan = {}
        stages_have_enabled_streaks = any(
            getattr(stage, 'streak_status', 'No') != 'Off'
            for stage in self.stages
        )
        self.enable_stages = False
        self.stages = []
        self.combine_stage_mindmaps = False
        if not stages_have_enabled_streaks:
            self.streak_status = 'Off'
        elif self.streak_status == 'Off':
            self.streak_status = 'No'
        self.get_today_goal_value()

    def _merged_stage_streaks(self):
        seen = set()
        merged = []
        for stage in getattr(self, 'stages', []):
            for entry in getattr(stage, 'streaks', []):
                key = (entry, len(merged)) if entry == STREAK_FREEZE_MARKER else entry
                if key in seen:
                    continue
                seen.add(key)
                merged.append(entry)
        return merged

    def get_stage_with_longest_streak(self):
        if not self.has_stages():
            return None

        return max(
            self.stages,
            key=lambda stage: streak_length(getattr(stage, 'streaks', [])),
            default=None
        )

    def transfer_longest_stage_streak(self, stage_with_longest_streak=None):
        """Переносит самый длинный стрик этапа в родительский проект."""
        if not self.has_stages():
            return

        if stage_with_longest_streak is None:
            stage_with_longest_streak = self.get_stage_with_longest_streak()
        if stage_with_longest_streak is None:
            return

        longest_streak = list(getattr(stage_with_longest_streak, 'streaks', []))
        if streak_length(longest_streak) > streak_length(self.streaks):
            self.streaks = longest_streak
            self.streak_status = getattr(stage_with_longest_streak, 'streak_status', 'Active')
        self.max_streak = max(
            self.max_streak,
            max([getattr(stage, 'max_streak', 0) for stage in self.stages] + [streak_length(self.streaks)])
        )

        for stage in self.stages:
            stage.streaks = []
            stage.streak_status = 'No'
            stage.last_streak_bonus = None

    def get_statistic(self):
        """
        Считает и возвращает статистику по проекту в виде словаря.
        Все параметры вычисляются из существующих данных без изменения структуры.
        """

        today = today_for_test()

        # --- Вспомогательные структуры ---
        # Группируем символы по дате
        symbols_by_date = defaultdict(float)
        notes_for_stats = [note for _, note in self.get_notes_with_stage_names()]
        for note in notes_for_stats:
            symbols_by_date[note.get_date_create()] += note.get_added_symbols()

        active_days = sorted(symbols_by_date.keys())
        total_notes = len(notes_for_stats)
        num_active_days = len(active_days)

        # --- 1. Кол-во записей ---
        stat_notes_count = total_notes

        # --- 2. Всего написано в единице проекта ---
        stat_total_in_unit = self.total_units

        # --- 3. Среднее символов в день (только по активным дням) ---
        if num_active_days > 0:
            total_sym = self.get_total_symbols()  # в символах
            stat_avg_symbols_per_active_day = round(total_sym / num_active_days)
        else:
            stat_avg_symbols_per_active_day = 0

        # --- 4. Среднее кол-во символов в записи ---
        if total_notes > 0:
            total_added = sum(note.get_added_symbols() for note in notes_for_stats)
            stat_avg_symbols_per_note = round(total_added / total_notes)
        else:
            stat_avg_symbols_per_note = 0

        # --- 5. Среднее кол-во записей в день (по активным дням) ---
        if num_active_days > 0:
            stat_avg_notes_per_day = round(total_notes / num_active_days, 1)
        else:
            stat_avg_notes_per_day = 0

        # --- 6. Использовано заморозок ---
        stat_freezes_used = self.freezes

        # --- 7. Лучший день (максимум символов за одну дату) ---
        if symbols_by_date:
            best_date = max(symbols_by_date, key=symbols_by_date.get)
            best_value = round(symbols_by_date[best_date])
            stat_best_day = f'{best_date.strftime("%d.%m.%Y")} — {best_value} симв.'
        else:
            stat_best_day = '—'

        # --- 8. Самый продуктивный день недели ---
        DAYS_RU = {
            0: 'Понедельник', 1: 'Вторник', 2: 'Среда',
            3: 'Четверг', 4: 'Пятница', 5: 'Суббота', 6: 'Воскресенье'
        }
        if symbols_by_date:
            symbols_by_weekday = defaultdict(float)
            for d, sym in symbols_by_date.items():
                symbols_by_weekday[d.weekday()] += sym
            best_weekday = max(symbols_by_weekday, key=symbols_by_weekday.get)
            stat_best_weekday = DAYS_RU[best_weekday]
        else:
            stat_best_weekday = '—'

        # --- 9. Текущий стрик (дней) ---
        stat_current_streak = streak_length(self.streaks)

        # --- 10. Максимальный стрик ---
        # max_streak обновляется в get_streak_status, берём максимум с текущим
        stat_max_streak = max(self.max_streak, stat_current_streak)

        # --- 11. Дней с начала проекта ---
        if self.create_date:
            stat_days_since_start = (today - self.create_date).days + 1
        else:
            stat_days_since_start = 0

        # --- 12. Активных дней (было хоть одна запись) ---
        stat_active_days_count = num_active_days

        # --- 13. Процент активных дней от общего времени проекта ---
        if stat_days_since_start > 0:
            stat_active_days_percent = round(
                stat_active_days_count / stat_days_since_start * 100, 1
            )
        else:
            stat_active_days_percent = 0

        return {
            'Кол-во записей': stat_notes_count,
            'Всего написано в единице проекта': stat_total_in_unit,
            'Среднее символов в день': stat_avg_symbols_per_active_day,
            'Среднее кол-во символов в записи': stat_avg_symbols_per_note,
            'Среднее кол-во записей в день': stat_avg_notes_per_day,
            'Использовано заморозок': stat_freezes_used,
            'Лучший день': stat_best_day,
            'Самый продуктивный день недели': stat_best_weekday,
            'Текущий стрик (дней)': stat_current_streak,
            'Максимальный стрик': stat_max_streak,
            'Дней с начала проекта': stat_days_since_start,
            'Активных дней': stat_active_days_count,
            'Процент активных дней': f'{stat_active_days_percent}%',
        }
class Stage(Project):
    def __init__(self, name='Этап 1', goal=None,
                 create_date=None, total_symbols=0, progress=0,
                 notes=None, streaks=None, max_streak=None, streak_status='No', deadline='Нет',
                 status='активен', unit='symbols', personal_goal_for_the_day=0,
                 parent_project_name=None, stage_id=None, auto_freeze=True):
        super().__init__(
            name=name,
            goal=goal,
            create_date=create_date,
            total_symbols=total_symbols,
            progress=progress,
            notes=notes,
            streaks=streaks,
            max_streak=max_streak,
            streak_status=streak_status,
            deadline=deadline,
            status=status,
            unit=unit,
            personal_goal_for_the_day=personal_goal_for_the_day,
            auto_freeze=auto_freeze,
        )
        self.is_stage = True
        self.enable_stages = False
        self.stages = []
        self.combine_stage_mindmaps = False
        self.parent_project_name = parent_project_name
        self.stage_id = stage_id or uuid.uuid4().hex

    def migrate(self):
        super().migrate()
        self.is_stage = True
        self.enable_stages = False
        self.stages = []
        self.combine_stage_mindmaps = False
        if not hasattr(self, 'stage_id') or not self.stage_id:
            self.stage_id = uuid.uuid5(
                uuid.NAMESPACE_URL,
                (
                    f'nfprogress-stage:{self.project_id}:{self.name}:'
                    f'{getattr(self, "create_date", None)}'
                ),
            ).hex
        if not hasattr(self, 'parent_project_name'):
            self.parent_project_name = None


def get_completion_entities(project):
    """Возвращает сущности, которые нужно завершить вместе с выбранной."""
    if isinstance(project, Stage) or getattr(project, 'is_stage', False):
        return [project]
    if getattr(project, 'has_stages', lambda: False)():
        return [stage for stage in project.stages if stage.status != 'завершен'] + [project]
    return [project]

class Note:

    def __init__(
            self, new_total, added_symbols, added_progress, date_create=None,
            entry_id=None, writing_day=None):
        if date_create is None:
            now = datetime.now()
            today = today_for_test()
            self.date_create = datetime(
                year=today.year,
                month=today.month,
                day=today.day,
                hour=now.hour,
                minute=now.minute
            )
        elif isinstance(writing_day, date):
            self.date_create = datetime.combine(
                writing_day,
                date_create.time().replace(tzinfo=None),
            )
        else:
            # Explicit timestamps arrive from synchronization sources with
            # their calendar date. Store them in the same writing-day form as
            # regular progress entries, otherwise work made before the custom
            # day start is excluded from today's amount and shifts the plan by
            # one daily step.
            self.date_create = writing_day_timestamp(date_create)

        self.new_total = new_total
        self.added_symbols = added_symbols
        self.added_progress = added_progress
        self.entry_id = entry_id or uuid.uuid4().hex

    def get_new_total(self):
        return self.new_total

    def get_added_symbols(self):
        return self.added_symbols

    def get_added_progress(self):
        return self.added_progress

    def get_date_create(self):
        return self.date_create.date()

    def get_date_create_str(self):
        return self.date_create.strftime('%d.%m.%Y %H:%M')

class Notification:
    def __init__(self, text, tag=None, date_create=None, status='New'):
        self.text = text
        if date_create is None:
            now = datetime.now()
            today = today_for_test()
            self.date_create = datetime(
                year=today.year,
                month=today.month,
                day=today.day,
                hour=now.hour,
                minute=now.minute
            )
        else:
            self.date_create = date_create
        self.status = status
        self.tag = tag

    def get_date_create(self):
        return self.date_create.strftime('%H:%M %d.%m.%y')

    def get_status(self):
        return self.status

    def set_status(self, status):
        self.status = status

    def get_text(self):
        return self.text

    def set_text(self, text):
        self.text = text


# === ЗАГРУЗКА И СОХРАНЕНИЕ ===

def load_data():
    """Загружает данные из кроссплатформенной директории"""
    data_file = get_data_file_path('data')
    try:
        with open(data_file, 'rb') as f:
            data = pickle.load(f)

        # Миграция всех проектов - добавление недостающих атрибутов
        projects = data.get('projects', {})
        for project_name, project in projects.items():
            if isinstance(project, Project):
                # Вызываем метод миграции для каждого проекта
                project.migrate()

        return data

    except (FileNotFoundError, EOFError):
        # Если файл не найден, создаём пустую структуру
        return {
            'last': None,
            'projects': {},
            'notifications': [],
            'global_streaks': [],
            'global_streak_status': 'No',
            'max_global_streak': 0,
            'last_global_streak_bonus': None,
            'last_global_streak_lost_date': None,
        }

def save_data(data):
    """Сохраняет данные в кроссплатформенную директорию"""
    data_file = get_data_file_path('data')
    atomic_pickle_save(data, data_file)
    _sync_shadow_after_legacy_save(data_file)


def apply_project_freeze(
        project, freeze_day=None, gamer=None, save_gamer=True, require_auto_freeze=False):
    """Применяет заморозку к проекту и списывает её из инвентаря игрока."""
    if freeze_day is None:
        freeze_day = today_for_test()

    if not isinstance(project, Project):
        return False
    if require_auto_freeze and not getattr(project, 'auto_freeze', True):
        return False
    if project.streak_status == 'Off' or project.status == 'завершен':
        return False
    if project.goal == float('inf') or project.get_today_goal_value() == 0:
        return False
    if not isinstance(project.streaks, list):
        project.streaks = []

    last_streak_day = streak_last_day(project.streaks)
    if last_streak_day != freeze_day - timedelta(days=1):
        return False
    if not load_settings().get('game_mode', False):
        return False

    batch = _project_freeze_batch if isinstance(project, Stage) else None
    parent_project = batch.get('parent') if batch else None
    batch_days = batch.get('spent_days') if batch else None
    batch_day_already_spent = batch_days is not None and freeze_day in batch_days

    if gamer is None and not batch_day_already_spent:
        import game
        gamer = game.load_game()

    if not batch_day_already_spent:
        items = getattr(gamer, 'items', {})
        category_items = items.get('Предметы', {})
        if not isinstance(category_items, dict):
            return False

        freeze_names = ('Заморозка', '❄️Заморозка')
        freeze_name = next((name for name in freeze_names if category_items.get(name, 0) > 0), None)
        if freeze_name is None:
            return False

        category_items[freeze_name] -= 1
        if batch_days is not None:
            batch_days.add(freeze_day)
        if isinstance(parent_project, Project):
            parent_project.freezes += 1

    project.streaks.append(STREAK_FREEZE_MARKER)
    if freeze_day == today_for_test():
        project.streak_status = 'Freeze'
    if batch is None:
        project.freezes += 1

    current_streak_len = streak_length(project.streaks)
    if current_streak_len > project.max_streak:
        project.max_streak = current_streak_len

    if save_gamer and not batch_day_already_spent:
        gamer.save()

    return True


def get_project_streak_sources(project):
    """Возвращает локальные стрики, которые участвуют в расчёте проекта."""
    if not isinstance(project, Project):
        return []

    project_streak_is_on = project.streak_status != 'Off'
    project_has_own_plan = (
        project.deadline != 'Нет'
        or bool(getattr(project, 'personal_goal_for_the_day', 0))
    )
    if not project.has_stages():
        return [project] if project_streak_is_on else []
    if project_streak_is_on and project_has_own_plan:
        return [project]
    return [
        stage
        for stage in project.stages
        if getattr(stage, 'streak_status', 'No') != 'Off'
    ]


def get_project_freeze_sources(project, freeze_day=None):
    """Возвращает стрики, которые можно заморозить в выбранном проекте."""
    if freeze_day is None:
        freeze_day = today_for_test()
    if not isinstance(project, Project) or project.status == 'завершен':
        return []

    sources = get_project_streak_sources(project)

    freeze_sources = []
    for source in sources:
        if not isinstance(source, Project):
            continue
        if not isinstance(getattr(source, 'streaks', None), list):
            continue
        if streak_last_day(source.streaks) != freeze_day - timedelta(days=1):
            continue
        status = source.get_streak_status()
        if status == 'Active':
            freeze_sources.append(source)

    return freeze_sources


def can_freeze_global_streak(data, freeze_day=None):
    """Проверяет, можно ли заморозить глобальный стрик без заморозки проекта."""
    if freeze_day is None:
        freeze_day = today_for_test()
    if not load_settings().get('game_mode', False):
        return False

    global_streaks = data.get('global_streaks', [])
    if not isinstance(global_streaks, list):
        return False
    if streak_last_day(global_streaks) != freeze_day - timedelta(days=1):
        return False

    projects = data.get('projects', {})
    return not any(
        get_project_freeze_sources(project, freeze_day)
        for project in projects.values()
        if isinstance(project, Project)
    )


def apply_global_streak_freeze(data, freeze_day=None, gamer=None, save_gamer=True):
    """Замораживает глобальный стрик и списывает один предмет из инвентаря."""
    if freeze_day is None:
        freeze_day = today_for_test()
    if not can_freeze_global_streak(data, freeze_day):
        return False

    if gamer is None:
        import game
        gamer = game.load_game()
    items = getattr(gamer, 'items', {})
    category_items = items.get('Предметы', {})
    if not isinstance(category_items, dict):
        return False

    freeze_names = ('Заморозка', '❄️Заморозка')
    freeze_name = next((name for name in freeze_names if category_items.get(name, 0) > 0), None)
    if freeze_name is None:
        return False

    category_items[freeze_name] -= 1
    if save_gamer:
        gamer.save()
    data['global_streaks'].append(STREAK_FREEZE_MARKER)
    data['global_streak_status'] = 'Freeze'
    return True


def apply_project_freeze_group(project, freeze_day=None, gamer=None, save_gamer=True):
    """Замораживает выбранный проект: родителя целиком или все активные этапы одним расходом."""
    if freeze_day is None:
        freeze_day = today_for_test()
    freeze_sources = get_project_freeze_sources(project, freeze_day)
    if not freeze_sources:
        return False

    if freeze_sources != [project]:
        if not load_settings().get('game_mode', False):
            return False

        if gamer is None:
            import game
            gamer = game.load_game()
        items = getattr(gamer, 'items', {})
        category_items = items.get('Предметы', {})
        if not isinstance(category_items, dict):
            return False

        freeze_names = ('Заморозка', '❄️Заморозка')
        freeze_name = next((name for name in freeze_names if category_items.get(name, 0) > 0), None)
        if freeze_name is None:
            return False

        applied_sources = []
        for source in freeze_sources:
            if streak_last_day(source.streaks) != freeze_day - timedelta(days=1):
                continue
            if streak_is_freeze_for_day(source.streaks, freeze_day):
                continue

            source.streaks.append(STREAK_FREEZE_MARKER)
            if freeze_day == today_for_test():
                source.streak_status = 'Freeze'

            current_streak_len = streak_length(source.streaks)
            if current_streak_len > source.max_streak:
                source.max_streak = current_streak_len

            applied_sources.append(source)

        if not applied_sources:
            return False

        category_items[freeze_name] -= 1
        project.freezes += 1
        if save_gamer:
            gamer.save()
        return True

    return apply_project_freeze(project, freeze_day, gamer=gamer, save_gamer=save_gamer)


def export_data_to_file(file_path):
    """Экспортирует данные в указанный файл"""
    data = load_data()
    try:
        with open(file_path, 'wb') as f:
            pickle.dump(data, f)
        return True, "Данные успешно экспортированы"
    except Exception as e:
        return False, f"Ошибка экспорта: {e}"


def import_data_from_file(file_path):
    """Импортирует данные из указанного файла"""
    try:
        with open(file_path, 'rb') as f:
            data = pickle.load(f)
        save_data(data)
        return True, "Данные успешно импортированы"
    except Exception as e:
        return False, f"Ошибка импорта: {e}"


def get_data_directory_info():
    """Возвращает информацию о директории с данными (для отладки)"""
    data_dir = get_app_data_dir()
    data_file = get_data_file_path('data')

    return {
        'system': SYSTEM,
        'data_dir': str(data_dir),
        'data_file': str(data_file),
        'data_dir_exists': data_dir.exists(),
        'data_file_exists': data_file.exists(),
        'data_file_size': data_file.stat().st_size if data_file.exists() else 0
    }

def global_streak_status(data, today=None, *, force=False):
    """Возвращает статус глобального стрика. Статусы потери возвращаются только в день потери."""
    if today is None:
        today = today_for_test()
    yesterday = today - timedelta(days=1)

    streaks = data.get('global_streaks', [])
    if streaks:
        # Чистим только повторные даты. Маркеры заморозки могут повторяться подряд.
        seen_dates = set()
        normalized_streaks = []
        for streak in streaks:
            if is_streak_date(streak):
                if streak in seen_dates:
                    continue
                seen_dates.add(streak)
            normalized_streaks.append(streak)
        streaks = normalized_streaks
        data['global_streaks'] = streaks
    if not isinstance(streaks, list):
        streaks = []
        data['global_streaks'] = streaks

    max_streak = data.get('max_global_streak', 0)
    prev_status = data.get('global_streak_status', 'No')
    last_lost_date = data.get('last_global_streak_lost_date')
    last_lost_len = data.get('last_global_streak_lose_len', 0)
    projects = data.get('projects', {})

    # Не пересчитываем стрик назад при изменении времени начала суток после
    # полуночи. Иначе сохранённая за текущую календарную дату запись выглядит
    # как дата из будущего и ошибочно трактуется как разрыв.
    if (not force and (
            streak_has_day_after(streaks, today) or
            any(streak_has_day_after(source.streaks, today)
                for source in _iter_streak_sources(projects)))):
        return prev_status

    if (not streaks and
            isinstance(prev_status, str) and
            prev_status.startswith('Lose ') and
            last_lost_date is not None and
            last_lost_date >= STREAK_DATE_BUG_START):
        repair_len = last_lost_len or max_streak
        if repair_len > 0:
            streaks = rebuild_streak_ending_at(STREAK_DATE_BUG_LAST_SAFE_DAY, repair_len)
            data['global_streaks'] = streaks
            data['global_streak_status'] = 'Active'
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
            prev_status = 'Active'
            last_lost_date = None
            last_lost_len = 0

    # Заморозка
    if prev_status == 'Freeze' and streak_is_freeze_for_day(streaks, today):
        return 'Freeze'

    # Определяем, есть ли сегодня проекты, выполнившие план и есть ли активные
    has_active_today = False
    for project in _iter_streak_sources(projects):
        if isinstance(project, Project):
            # ВАЖНО: обязательно вызываем метод для актуализации статуса на текущий день
            actual_status = project.get_streak_status(force=force)

            # Проект активен или завершен
            if project.status in ['активен', 'завершен']:
                # Стрик начат/продлен
                if actual_status in ['Start', 'Go']:
                    has_active_today = True
                # Стрик завершен сегодня
                elif actual_status == 'Complete' and streak_last_day(project.streaks) == today:
                    has_active_today = True

                # Перенимаем стрик, если он активен и длиннее текущего глобального (используем copy!)
                if actual_status in ['Start', 'Go', 'Active'] and streak_length(streaks) < streak_length(project.streaks):
                    streaks = project.streaks.copy()

    repair_streak_gap_after_date_bug(streaks, yesterday)
    data['global_streaks'] = streaks

    # === ОБРАБОТКА СОХРАНЁННОГО СТАТУСА ПОТЕРИ (только в день потери) ===
    if (isinstance(prev_status, str) and
            prev_status.startswith('Lose ') and
            len(prev_status.split()) == 2 and
            last_lost_date == today):
        # Автозаморозка могла восстановить вчерашний день уже после того,
        # как потеря была сохранена. В этом случае старый Lose больше не валиден.
        if streak_last_day(streaks) == yesterday:
            status = 'Active'
            data['global_streaks'] = streaks
            data['global_streak_status'] = status
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
            data['max_global_streak'] = max(streak_length(streaks), max_streak)
            save_data(data)
            return status

        # Если сегодня нет активности — оставляем статус потери
        if not has_active_today:
            return prev_status
        else:
            # Появилась активность в день потери — переходим к комбинированному статусу
            streaks = [today]
            status = f'Lose {last_lost_len} Start'
            data['global_streaks'] = streaks
            data['global_streak_status'] = status
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
            data['max_global_streak'] = max(1, max_streak)
            save_data(data)
            return status

    # === ОБРАБОТКА КОМБИНИРОВАННОГО СТАТУСА (только в день его установки) ===
    if (isinstance(prev_status, str) and
            prev_status.startswith('Lose ') and
            len(prev_status.split()) == 3 and
            streak_last_day(streaks) == today):
        # Это комбинированный статус, установленный сегодня — возвращаем его
        return prev_status

    # Если сохранённый статус потери относится к прошлому дню, сбрасываем его в 'No'
    if (isinstance(prev_status, str) and
            prev_status.startswith('Lose ') and
            len(prev_status.split()) == 2 and
            last_lost_date is not None and
            last_lost_date < today):
        # Прошёл день потери без активности — сбрасываем в No
        data['global_streak_status'] = 'No'
        data['last_global_streak_lost_date'] = None
        data['last_global_streak_lose_len'] = 0
        save_data(data)
        prev_status = 'No'

    # === ОСНОВНАЯ ЛОГИКА РАСЧЁТА СТАТУСА ===
    if has_active_today:
        if not streaks:
            # Новый стрик
            streaks.append(today)
            if last_lost_date is not None:
                # Если была зафиксирована потеря (например, вчера), но сегодня активность — комбо
                status = f'Lose {last_lost_len} Start'
                data['last_global_streak_lost_date'] = None
                data['last_global_streak_lose_len'] = 0
            else:
                status = 'Start'
        elif streak_last_day(streaks) == yesterday:
            # Продолжение стрика
            streaks.append(today)
            status = 'Go'
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
        elif streak_last_day(streaks) == today:
            # ИСПРАВЛЕНИЕ: Стрик уже обновлён сегодня (глобально ранее, либо только что переняли от проекта).
            # Вычисляем статус вместо возврата старого prev_status.
            if streak_length(streaks) > 1:
                status = 'Go'
            else:
                if last_lost_date is not None:
                    status = f'Lose {last_lost_len} Start'
                else:
                    status = 'Start'
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
        else:
            # Пропуск дней: потеря старого стрика и начало нового
            lose_len = streak_length(streaks)
            if lose_len > max_streak:
                max_streak = lose_len
            streaks = [today]
            status = f'Lose {lose_len} Start'
            data['last_global_streak_lost_date'] = None
            data['last_global_streak_lose_len'] = 0
    else:
        # Нет активности сегодня
        if not streaks:
            status = 'No'
        elif streak_last_day(streaks) == yesterday:
            status = 'Active'
        elif streak_last_day(streaks) == today:
            # Запись за сегодня есть, но план не выполнен (находится в процессе) — статус активен
            status = 'Active'
        else:
            # Потеря стрика из-за пропуска более одного дня
            lose_len = streak_length(streaks)
            if lose_len > max_streak:
                max_streak = lose_len
            streaks.clear()
            status = f'Lose {lose_len}'
            data['last_global_streak_lost_date'] = today
            data['last_global_streak_lose_len'] = lose_len

    # Обновление максимальной длины стрика
    current_len = streak_length(streaks)
    if current_len > max_streak:
        max_streak = current_len

    data['global_streaks'] = streaks
    data['max_global_streak'] = max_streak
    data['global_streak_status'] = status
    save_data(data)
    return status


def _iter_streak_sources(projects):
    for project in projects.values():
        if not isinstance(project, Project):
            continue
        yield from get_project_streak_sources(project)


def refresh_project_streak_statuses(data, *, force=False):
    """Актуализирует локальные стрики проектов без UI-уведомлений."""
    projects = data.get('projects', {})
    global_streaks = data.setdefault('global_streaks', [])
    if not isinstance(global_streaks, list):
        global_streaks = []
        data['global_streaks'] = global_streaks
    changed = False
    freeze_changed = False

    for project_name, project in projects.items():
        if not isinstance(project, Project):
            continue
        streak_sources = get_project_streak_sources(project)
        source_changed = False
        old_project_freezes = getattr(project, 'freezes', 0)
        previous_batch = (
            begin_project_freeze_batch(project)
            if streak_sources and streak_sources != [project]
            else None
        )

        try:
            for streak_source in streak_sources:
                old_streaks = list(streak_source.streaks) if isinstance(streak_source.streaks, list) else []
                old_status = streak_source.streak_status
                old_freezes = getattr(streak_source, 'freezes', 0)
                old_max_streak = getattr(streak_source, 'max_streak', 0)
                old_freeze_count = sum(1 for entry in old_streaks if entry == STREAK_FREEZE_MARKER)

                streak_source.get_streak_status(force=force)

                freeze_days = [
                    effective_day
                    for entry, effective_day in iter_streak_days(streak_source.streaks)
                    if entry == STREAK_FREEZE_MARKER
                ]
                new_freeze_days = freeze_days[old_freeze_count:]
                if new_freeze_days:
                    freeze_changed = True
                    for freeze_day in new_freeze_days:
                        if streak_last_day(global_streaks) == freeze_day - timedelta(days=1):
                            global_streaks.append(STREAK_FREEZE_MARKER)
                            data['global_streak_status'] = 'Freeze'
                            changed = True
                if (
                        streak_source.streaks != old_streaks
                        or streak_source.streak_status != old_status
                        or getattr(streak_source, 'freezes', 0) != old_freezes
                        or getattr(streak_source, 'max_streak', 0) != old_max_streak
                        or global_streaks != data.get('global_streaks', [])
                ):
                    source_changed = True
        finally:
            if previous_batch is not None:
                end_project_freeze_batch(previous_batch)

        if getattr(project, 'freezes', 0) != old_project_freezes:
            source_changed = True
        if source_changed:
            changed = True
            data['projects'][project_name] = project

    # Как и проектные стрики, глобальный стрик автозамораживается только
    # при обработке уже пропущенного дня, а не в день, когда он ещё активен.
    missed_day = today_for_test() - timedelta(days=1)
    if apply_global_streak_freeze(data, missed_day, gamer=None):
        changed = True
        freeze_changed = True

    if changed:
        save_data(data)

    return {'changed': changed, 'freeze_changed': freeze_changed}


def global_streak_status_msg(data, status=None):
    """Сообщение для глобального стрика по статусу."""
    if status is None:
        status = data.get('global_streak_status', 'No')

    streaks = data.get('global_streaks', [])

    if status == 'Start':
        return '🔥 Глобальный стрик начат!'
    elif status == 'Go':
        return f'🚀 Глобальный стрик продлен! Дней подряд: {streak_length(streaks)}'
    elif status == 'Freeze':
        return f'❄️ Глобальный стрик в {streak_length(streaks)} д. заморожен'
    elif status == 'Active':
        # Активный глобальный стрик (вчера продлен, сегодня не продлен)
        return f'👀 Глобальный стрик в {streak_length(streaks)} д. не продлен сегодня.'
    elif isinstance(status, str) and status.startswith('Lose '):
        parts = status.split()
        if len(parts) == 2 and parts[1].isdigit():
            return f'💔 Глобальный стрик потерян! Было дней подряд: {parts[1]}'
        elif len(parts) == 3:
            # Комбинированное сообщение: потеря + новый старт
            return (f'💔 Глобальный стрик потерян! Было дней подряд: {parts[1]}'
                    f'\n🔥 Вы начали новый стрик!')
        return '💔 Глобальный стрик потерян!'
    elif status == 'No':
        # Если нет активных стриков, но были раньше
        last_streak_day = streak_last_day(streaks)
        if last_streak_day is not None:
            today = today_for_test()
            days_ago = (today - last_streak_day).days
            if days_ago == 1:
                return f'👀 Глобальный стрик в {streak_length(streaks)} д. не продлен.'
            else:
                return f'😴 Глобальный стрик не активен. Последний стрик был {days_ago} дней назад'
        else:
            return '😴 Глобальный стрик не начат'

    return '😴 Глобальный стрик не начат'

def unit_converter(unit, value, convert_to=None):
    """
    Конвертирует количество value из исходной единицы unit в целевую единицу convert_to.
    """
    factors = {
        'symbols': 1,
        'A4': 1800,
        'author_list': 40000,
        'ficbook_pages': 4500
    }

    if convert_to in (None, False):
        convert_to = 'symbols'

    if unit not in factors or convert_to not in factors:
        return None

    # Приводим к общему знаменателю (символам) и вычисляем результат
    symbols_value = value * factors[unit]
    result = symbols_value / factors[convert_to]

    # Логика округления в зависимости от типа единицы
    if convert_to == 'symbols':
        return result
    elif convert_to == 'author_list':
        # Для авторских листов — 1 знак после запятой
        return round(result, 1)
    else:
        # Для страниц (A4, Ficbook) оставляем округление вверх до целого
        return math.ceil(result)

def count_symbols_in_docx(filepath):
    """
    Возвращает общее количество символов (с пробелами) в тексте документа .docx.
    Учитываются абзацы и текст в таблицах.
    """

    # python-docx loads lxml and several megabytes of parser state. Word
    # synchronization is optional, so keep it out of the normal startup path.
    from docx import Document

    doc = Document(filepath)
    total = 0
    for paragraph in doc.paragraphs:
        total += len(paragraph.text)
    for table in doc.tables:
        for row in table.rows:
            for cell in row.cells:
                for paragraph in cell.paragraphs:
                    total += len(paragraph.text)
    return total

# Persistent test_data обновляется только явной командой refresh.
if not dev_mode:
    get_app_data_dir()
