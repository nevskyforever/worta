import os
import pickle
import sys
import math
import random
from contextlib import contextmanager
from contextvars import ContextVar
from datetime import date, datetime, timedelta
from pathlib import Path

import engine
import game_data


_SUPPRESS_PERSISTENCE = ContextVar('nfprogress_suppress_game_persistence', default=False)
_RUNTIME_DATA = ContextVar('nfprogress_runtime_game_data', default=None)
_RUNTIME_SETTINGS = ContextVar('nfprogress_runtime_game_settings', default=None)


@contextmanager
def suppress_persistence():
    """Prevent compatibility rules from committing intermediate snapshots."""
    token = _SUPPRESS_PERSISTENCE.set(True)
    try:
        yield
    finally:
        _SUPPRESS_PERSISTENCE.reset(token)


@contextmanager
def runtime_data_context(data, settings=None):
    """Provide project data to compatibility rules without a PKL read."""
    data_token = _RUNTIME_DATA.set(data)
    settings_token = _RUNTIME_SETTINGS.set(settings)
    try:
        with engine.runtime_settings_context(settings):
            yield
    finally:
        _RUNTIME_SETTINGS.reset(settings_token)
        _RUNTIME_DATA.reset(data_token)


def get_runtime_data():
    """Return the current service-bound project data, if any."""
    return _RUNTIME_DATA.get()


def get_runtime_settings():
    """Return the current service-bound settings, if any."""
    return _RUNTIME_SETTINGS.get()


CF_META = {
    'coins': {
        'name': 'Монеты',
        'default': 1.0,
        'description': 'Коэффициент награды монетами. За 100 символов вы получите {coins_per_100} монет.',
    },
    'exp': {
        'name': 'Опыт',
        'default': 1.0,
        'description': 'Коэффициент награды опытом. За 100 символов вы получите {exp_per_100} опыта.',
    },
    'health_recovery': {
        'name': 'Восстановление',
        'default': 0.0,
        'description': 'Коэффициент восстановления здоровья. Сейчас восстанавливает {health_recovery_per_hour:g} здоровья в час.',
    },
}

SKILL_POINTS_PER_LEVEL = 2
SKILL_CF_STEP = 0.25
BASE_MAX_HEALTH = 100
MAX_HEALTH_PER_5_LEVELS = 10
MAX_INSPIRATION = 100
INSPIRATION_SYMBOL_STEP = 500
SPECIALIZATION_LEVEL = 3
SPECIALIZATION_CHANGE_COOLDOWN_DAYS = 14
SPECIALIZATION_MASTERY_THRESHOLDS = (0, 3, 8, 15, 25)
SPECIALIZATION_ABILITY_COOLDOWN_HOURS = 24

SPECIALIZATIONS = {
    'marathoner': {
        'name': 'Марафонец',
        'description': 'Даёт +15% монет и опыта за записи от 3 000 символов.',
        'base_bonus': 0.15,
        'mastery_step': 0.025,
    },
    'ritualist': {
        'name': 'Ритуалист',
        'description': 'Даёт +25% к награде за успешную писательскую сессию.',
        'base_bonus': 0.25,
        'mastery_step': 0.025,
    },
    'finisher': {
        'name': 'Финишер',
        'description': 'Даёт +20% к награде за завершение этапа или проекта.',
        'base_bonus': 0.20,
        'mastery_step': 0.025,
    },
    'explorer': {
        'name': 'Исследователь',
        'description': 'Даёт +20% к наградам за дневные и недельные испытания.',
        'base_bonus': 0.20,
        'mastery_step': 0.025,
    },
    'editor': {
        'name': 'Редактор',
        'description': 'Даёт +25% к награде за успешную редакторскую сессию.',
        'base_bonus': 0.25,
        'mastery_step': 0.025,
    },
}

SPECIALIZATION_ABILITIES = {
    'marathoner': {
        'name': 'Длинное дыхание',
        'description': 'Даёт +30% к следующей записи объёмом не менее 3 000 символов.',
        'bonus': 0.30,
    },
    'ritualist': {
        'name': 'Сила ритуала',
        'description': 'Даёт +30% к следующей успешной сессии или один раз сохраняет серию при неудаче.',
        'bonus': 0.30,
    },
    'finisher': {
        'name': 'Рывок к финалу',
        'description': 'Даёт +30% к наградам за следующие достигнутые рубежи одной рукописи.',
        'bonus': 0.30,
    },
    'explorer': {
        'name': 'Новый маршрут',
        'description': 'Бесплатно заменяет текущее недельное испытание другим.',
        'bonus': 0.0,
    },
    'editor': {
        'name': 'Точный взгляд',
        'description': 'Даёт +30% к награде за следующую успешную редакторскую сессию.',
        'bonus': 0.30,
    },
}

MANUSCRIPT_MILESTONES = (
    {'progress': 10, 'name': 'Искра замысла', 'coins': 25, 'exp': 250, 'inspiration': 2},
    {'progress': 25, 'name': 'Первые главы', 'coins': 50, 'exp': 500, 'inspiration': 3},
    {'progress': 50, 'name': 'Переломная точка', 'coins': 100, 'exp': 1000, 'inspiration': 5},
    {'progress': 75, 'name': 'Финишная прямая', 'coins': 150, 'exp': 1500, 'inspiration': 7},
    {'progress': 90, 'name': 'Почти готово', 'coins': 200, 'exp': 2000, 'inspiration': 8},
    {'progress': 100, 'name': 'Рукопись завершена', 'coins': 300, 'exp': 3000, 'inspiration': 10},
)

CABINET_RELICS = {
    'ink_candle': {
        'name': 'Чернильная свеча',
        'description': 'Маленький огонь, зажжённый первой настоящей работой над рукописью.',
        'condition': 'Достигните рубежа 10% в одном тексте.',
        'required_progress': 10,
        'required_projects': 1,
        'effect_type': 'writing',
        'bonus': 0.01,
        'effect_description': 'Даёт +1% к наградам за написанный текст.',
    },
    'plot_map': {
        'name': 'Карта сюжетных поворотов',
        'description': 'Карта пройденного пути через середину большой истории.',
        'condition': 'Достигните рубежа 50% в одном тексте.',
        'required_progress': 50,
        'required_projects': 1,
        'effect_type': 'challenge',
        'bonus': 0.03,
        'effect_description': 'Даёт +3% к наградам за испытания.',
    },
    'first_binding': {
        'name': 'Переплёт первой рукописи',
        'description': 'Память о тексте, который прошёл весь путь от замысла до финала.',
        'condition': 'Доведите один текст до 100%.',
        'required_progress': 100,
        'required_projects': 1,
        'effect_type': 'completion',
        'bonus': 0.05,
        'effect_description': 'Даёт +5% к наградам за завершение этапов и проектов.',
    },
    'chapter_shelf': {
        'name': 'Полка первых глав',
        'description': 'Место для историй, каждая из которых уже обрела собственный голос.',
        'condition': 'Доведите три разных текста минимум до 25%.',
        'required_progress': 25,
        'required_projects': 3,
        'effect_type': 'session',
        'bonus': 0.03,
        'effect_description': 'Даёт +3% к наградам за успешные сессии.',
    },
    'turning_quill': {
        'name': 'Перо переломного момента',
        'description': 'Перо, которым история была проведена через самый трудный поворот.',
        'condition': 'Достигните рубежа 75% в одном тексте.',
        'required_progress': 75,
        'required_projects': 1,
        'effect_type': 'milestone',
        'bonus': 0.05,
        'effect_description': 'Даёт +5% к наградам за рубежи рукописи.',
    },
    'final_lamp': {
        'name': 'Лампа финишной прямой',
        'description': 'Её свет помогает не потерять дорогу, когда до финала остаётся совсем немного.',
        'condition': 'Достигните рубежа 90% в одном тексте.',
        'required_progress': 90,
        'required_projects': 1,
        'effect_type': 'inspiration',
        'bonus': 0.10,
        'effect_description': 'Увеличивает получаемое за работу вдохновение на 10%.',
    },
    'triple_map': {
        'name': 'Атлас трёх миров',
        'description': 'Три истории на одной карте — доказательство широты авторской вселенной.',
        'condition': 'Доведите три разных текста минимум до 50%.',
        'required_progress': 50,
        'required_projects': 3,
        'effect_type': 'writing',
        'bonus': 0.02,
        'effect_description': 'Даёт +2% к наградам за написанный текст.',
    },
    'finished_shelf': {
        'name': 'Полка завершённых рукописей',
        'description': 'Полка для историй, которым автор подарил настоящий финал.',
        'condition': 'Доведите три разных текста до 100%.',
        'required_progress': 100,
        'required_projects': 3,
        'effect_type': 'completion',
        'bonus': 0.10,
        'effect_description': 'Даёт +10% к наградам за завершение этапов и проектов.',
    },
}

CABINET_SETS = {
    'manuscript_path': {
        'name': 'Путь большой рукописи',
        'description': 'Пять реликвий пути дают +3% ко всем наградам монетами и опытом.',
        'relics': (
            'ink_candle', 'plot_map', 'turning_quill', 'final_lamp', 'first_binding',
        ),
        'effect_type': 'all_rewards',
        'bonus': 0.03,
    },
    'authors_library': {
        'name': 'Авторская библиотека',
        'description': 'Три реликвии библиотеки увеличивают получаемое вдохновение на 20%.',
        'relics': ('chapter_shelf', 'triple_map', 'finished_shelf'),
        'effect_type': 'inspiration',
        'bonus': 0.20,
    },
}

WEEKLY_CHALLENGES = {
    'symbols': {
        'name': 'Марафон',
        'description': 'Написать 10 000 символов за неделю.',
        'target': 10000,
        'reward_coins': 500,
        'reward_exp': 1500,
    },
    'days': {
        'name': 'Ритм',
        'description': 'Писать в четыре разных дня за неделю.',
        'target': 4,
        'reward_coins': 400,
        'reward_exp': 1200,
    },
    'sessions': {
        'name': 'Чистый поток',
        'description': 'Завершить пять успешных писательских сессий.',
        'target': 5,
        'reward_coins': 450,
        'reward_exp': 1350,
    },
    'editing': {
        'name': 'Редакторская неделя',
        'description': 'Завершить три успешные сессии с намерением отредактировать текст.',
        'target': 3,
        'reward_coins': 425,
        'reward_exp': 1300,
    },
}

INSPIRATION_ABILITIES = {
    'creative_surge': {
        'name': 'Творческий импульс',
        'description': 'Даёт +25% монет и опыта за следующую запись текста.',
        'cost': 30,
        'bonus': 0.25,
        'bonus_field': 'writing_reward_bonus',
    },
    'session_spark': {
        'name': 'Искра сессии',
        'description': 'Даёт +25% к награде за следующую успешную писательскую сессию.',
        'cost': 25,
        'bonus': 0.25,
        'bonus_field': 'session_reward_bonus',
    },
    'challenge_focus': {
        'name': 'Фокус испытания',
        'description': 'Даёт +25% к награде за следующее выполненное испытание.',
        'cost': 40,
        'bonus': 0.25,
        'bonus_field': 'challenge_reward_bonus',
    },
}

WRITING_SESSION_MODES = {
    'sprint': {
        'name': 'Спринт',
        'description': '15 минут: +15% к награде за быстрый результат.',
        'reward_bonus': 0.15,
    },
    'flow': {
        'name': 'Поток',
        'description': 'Свободный сбалансированный режим без дополнительных условий.',
        'reward_bonus': 0.0,
    },
    'deep': {
        'name': 'Глубокая работа',
        'description': '45 или 60 минут: +25% к награде за длительную концентрацию.',
        'reward_bonus': 0.25,
    },
    'editing': {
        'name': 'Редакторский проход',
        'description': (
            'Учитывает добавленные и удалённые символы по модулю изменения '
            'и даёт +20% к награде за успешную сессию.'
        ),
        'reward_bonus': 0.20,
    },
}

WRITING_SESSION_GRADES = (
    ('gold', 'Золото', 1.50, 1.30),
    ('silver', 'Серебро', 1.25, 1.15),
    ('bronze', 'Бронза', 1.00, 1.00),
)
WRITING_SESSION_HISTORY_LIMIT = 20

DAILY_CHALLENGE_CHANGE_COST = 15
DAILY_CHALLENGE_TYPES = {
    'symbols': {
        'name': 'Объём дня',
        'description': 'Написать выбранный объём текста за день.',
    },
    'sessions': {
        'name': 'Сессионный ритм',
        'description': 'Завершить несколько успешных писательских сессий.',
    },
    'editing': {
        'name': 'День редактора',
        'description': 'Завершить успешные сессии с намерением отредактировать текст.',
    },
}
DAILY_CHALLENGE_DIFFICULTIES = {
    'easy': {'name': 'Легко', 'target_multiplier': 0.75, 'reward_multiplier': 0.8},
    'normal': {'name': 'Обычно', 'target_multiplier': 1.0, 'reward_multiplier': 1.0},
    'hard': {'name': 'Сложно', 'target_multiplier': 1.25, 'reward_multiplier': 1.4},
}

CREATIVE_EVENTS = {
    'unexpected_idea': {
        'name': 'Неожиданная идея',
        'description': 'В тексте появился новый перспективный поворот.',
        'safe_description': 'Записать идею и получить 6 вдохновения.',
        'risk_description': 'Развить идею: 55% получить 15 вдохновения, иначе потерять 5.',
        'safe': ('inspiration', 6),
        'risk': ('inspiration', 15, 'inspiration_loss', 5),
    },
    'second_wind': {
        'name': 'Второе дыхание',
        'description': 'Рабочий темп неожиданно стал легче и увереннее.',
        'safe_description': 'Сохранить темп: +10% к следующей записи.',
        'risk_description': 'Ускориться: 55% получить +30%, иначе потерять 8 вдохновения.',
        'safe': ('writing_bonus', 0.10),
        'risk': ('writing_bonus', 0.30, 'inspiration_loss', 8),
    },
    'lucky_find': {
        'name': 'Удачная находка',
        'description': 'В старых заметках нашлась полезная деталь для рукописи.',
        'safe_description': 'Использовать сразу и получить 50 монет.',
        'risk_description': 'Проверить редкую версию: 55% получить 150 монет.',
        'safe': ('coins', 50),
        'risk': ('coins', 150, 'nothing', 0),
    },
}
CREATIVE_EVENT_PRODUCTIVE_INTERVAL = 5

SKILL_META = {
    'productivity': {
        'name': 'Продуктивность',
        'target_cf': 'exp',
    },
    'profitability': {
        'name': 'Доходность',
        'target_cf': 'coins',
    },
    'endurance': {
        'name': 'Выносливость',
        'target_cf': 'health_recovery',
    },
}


class Quest:
    AVAILABLE = 'available'
    ACTIVE = 'active'
    COMPLETED = 'completed'

    def __init__(self, quest_id, name, description, reward_coins=0, reward_exp=0,
                 reward_items=None, reward_buffs=None, level=1, status=AVAILABLE,
                 quest_func=None, start_date=None, end_date=None):
        self.quest_id = quest_id
        self.name = name
        self.description = description
        self.reward_coins = reward_coins
        self.reward_exp = reward_exp
        self.reward_items = reward_items or []
        self.reward_buffs = reward_buffs or []
        # None means that the quest was saved before item rewards were added.
        # It is intentionally distinct from False so migration can compensate it.
        self.reward_items_received = False
        self.level = level
        self.status = status
        self.quest_func = quest_func
        self.start_date = start_date
        self.end_date = end_date

    def start(self, gamer):
        if self.status != self.AVAILABLE:
            return False, 'Квест нельзя начать.'
        if gamer.level < self.level:
            return False, f'Квест доступен с {self.level} уровня.'

        self.status = self.ACTIVE
        self.start_date = datetime.now()
        self.end_date = None
        return True, f'Квест "{self.name}" начат.'

    def check_conditions(self, gamer):
        if self.status != self.ACTIVE:
            return False

        quest_function = self.get_quest_function()
        if not quest_function:
            return False

        return bool(quest_function(gamer, self))

    def get_quest_function(self):
        if callable(self.quest_func):
            return self.quest_func
        if not self.quest_func:
            return None

        try:
            import gama_quests
        except ImportError:
            return None

        return getattr(gama_quests, self.quest_func, None)

    def complete(self, gamer):
        if self.status == self.COMPLETED:
            return None

        self.status = self.COMPLETED
        self.end_date = datetime.now()
        if self.reward_coins:
            gamer.set_coins(self.reward_coins, save=False)
        if self.reward_exp:
            gamer.add_exp(self.reward_exp)
        self.give_reward_items(gamer)
        self.reward_items_received = True
        self.give_reward_buffs(gamer)
        return f'Квест "{self.name}" завершен.\n{self.format_reward()}'

    def give_reward_items(self, gamer, skip_existing_awards=False):
        """Выдает предметы квеста и компенсирует переполнение ограниченного инвентаря."""
        granted_items = []
        compensated_coins = 0
        for reward_item in self.reward_items:
            category = reward_item.get('category', 'Награды')
            name = reward_item.get('name')
            count = int(reward_item.get('count', 1))
            if not name or count <= 0:
                continue
            registry_key, item = game_data.find_registry_item(category, name)
            name = registry_key or name
            gamer.items.setdefault(category, {})
            current_count = gamer.items[category].get(name, 0)

            if skip_existing_awards and category == 'Награды' and current_count > 0:
                continue

            granted_count = count
            maximum = getattr(item, 'maximum_quantity_in_stock', None)
            is_freeze = item is getattr(game_data, 'freeze', None)
            if maximum is not None and not is_freeze:
                granted_count = min(count, max(0, maximum - current_count))
                overflow_count = count - granted_count
                if overflow_count:
                    compensated_coins += item.sell_price * overflow_count

            if granted_count:
                gamer.items[category][name] = current_count + granted_count
                granted_items.append((item.name if item else name, granted_count))

        compensated_coins = gamer.round_money(compensated_coins)
        if compensated_coins:
            gamer.set_coins(compensated_coins, process_bank_events=False, save=False)
        return granted_items, compensated_coins

    def give_reward_buffs(self, gamer):
        for buff in self.reward_buffs:
            if buff:
                gamer.add_buff(buff, save=False)
        gamer.apply_buffs_to_cf(save=False)

    def can_be_available(self, gamer):
        return self.status == self.AVAILABLE and gamer.level >= self.level

    def format_reward(self):
        parts = []
        if self.reward_coins:
            parts.append(f'{self.reward_coins} монет')
        if self.reward_exp:
            parts.append(f'{self.reward_exp} опыта')
        for reward_item in self.reward_items:
            name = reward_item.get('name')
            count = reward_item.get('count', 1)
            if name:
                parts.append(f'{name} x{count}')
        for buff in self.reward_buffs:
            duration = self.format_buff_duration(buff)
            sign = '+' if buff.is_positive() else '-'
            target_name = self.get_buff_target_name(buff)
            parts.append(f'баф "{buff.name}" ({target_name} {sign}{abs(buff.value):g}, {duration})')
        if not parts:
            return 'Награда не указана'
        return 'Награда:\n' + '\n'.join(f'- {part}' for part in parts)

    def get_buff_target_name(self, buff):
        return CF_META.get(buff.target_cf, {}).get('name', buff.target_cf)

    def format_buff_duration(self, buff):
        if buff.duration_minutes is None:
            return 'бессрочно'
        if buff.duration_minutes % (24 * 60) == 0:
            return f'{buff.duration_minutes // (24 * 60)} д.'
        if buff.duration_minutes % 60 == 0:
            return f'{buff.duration_minutes // 60} ч.'
        return f'{buff.duration_minutes} мин.'

    def normalize(self):
        if not hasattr(self, 'reward_items') or self.reward_items is None:
            self.reward_items = []
        if not hasattr(self, 'reward_buffs') or self.reward_buffs is None:
            self.reward_buffs = []
        if not hasattr(self, 'reward_items_received'):
            self.reward_items_received = None
        if not hasattr(self, 'status') or self.status not in (self.AVAILABLE, self.ACTIVE, self.COMPLETED):
            self.status = self.AVAILABLE
        if not hasattr(self, 'start_date'):
            self.start_date = None
        if not hasattr(self, 'end_date'):
            self.end_date = None
        return self


def get_data_file_path():
    """Возвращает путь к файлу данных игры.

    В режиме разработчика файл хранится в папке test_data.
    """
    return engine.get_data_file_path('gamer')


def resource_path(relative_path):
    """Получить путь к ресурсу, работает и в .py, и в .app, и в .exe"""
    if hasattr(sys, '_MEIPASS'):
        # PyInstaller создает временную папку и хранит путь в _MEIPASS
        return os.path.join(sys._MEIPASS, relative_path)
    return str(Path(__file__).resolve().parent / relative_path)


def get_effective_now():
    """Возвращает текущее время с датой из режима разработчика, если она включена."""
    runtime_settings = get_runtime_settings()
    if runtime_settings is not None:
        if getattr(engine, 'dev_mode', False) and runtime_settings.get('today_for_test_mode', False):
            selected_datetime = runtime_settings.get('today_for_test_datetime')
            if isinstance(selected_datetime, datetime):
                return selected_datetime
            if isinstance(selected_datetime, str):
                try:
                    return as_local_naive_datetime(selected_datetime) or datetime.now()
                except ValueError:
                    pass
        return datetime.now()
    return as_local_naive_datetime(engine.now_for_test()) or datetime.now()


def as_local_naive_datetime(value):
    """Read a persisted timestamp in the naive local-time format used by the game."""
    try:
        parsed = datetime.fromisoformat(str(value))
    except (TypeError, ValueError):
        return None
    if parsed.tzinfo is not None:
        parsed = parsed.astimezone().replace(tzinfo=None)
    return parsed


def get_session_now():
    """Возвращает реальное время для живого таймера писательской сессии."""
    return datetime.now()


class Gamer:
    # === 2. ИНИЦИАЛИЗАЦИЯ ===
    def __init__(self, level=1, exp=0, coins=0, health=100):
        self.level = level
        self.exp = exp
        self.coins = self.round_money(coins)
        self.inflation = self.calculate_inflation()
        self.health = health
        self.max_health = self.calculate_max_health()

        self.cf = {
            'coins': self._make_cf_parameter('coins', 1.0),
            'exp': self._make_cf_parameter('exp', 1.0),
            'health_recovery': self._make_cf_parameter('health_recovery', 0.0),
        }
        self.skills = self.get_default_skills()
        self.available_skill_points = 0
        self.skill_points_awarded_for_level = level
        self.last_health_recovery_at = get_effective_now()
        self.items = {}
        self.custom_awards = []
        self.custom_awards_inventory = {}

        self.bank_account = game_data.BankAccount()
        self.last_lose_global_streak_damage = None
        self.last_bonus_dates = {}
        self.api_streak_reward_days = {}
        self.complete_bonus_projects = []
        self.buffs = []
        self.debuffs = []

        self.quests = []
        self.inspiration = 0
        self.daily_challenge = None
        self.daily_challenge_options = []
        self.daily_challenge_history = []
        self.weekly_challenge = None
        self.productive_actions_since_event = 0
        self.pending_creative_event = None
        self.creative_event_history = []
        self.writing_session = None
        self.writing_session_streak = 0
        self.writing_session_history = []
        self.session_streak_shields = 0
        self.session_grade_boosts = 0
        self.writing_reward_bonus = 0.0
        self.session_reward_bonus = 0.0
        self.challenge_reward_bonus = 0.0
        self.manuscript_reward_bonus = 0.0
        self.specialization = None
        self.specialization_changed_at = None
        self.specialization_mastery = {}
        self.specialization_ability_ready_at = {}
        self.specialization_ability_effects = {}
        self.manuscript_journeys = {}
        self.cabinet_relics = []
        self.sync_quests()

    def _make_cf_parameter(self, key, value, base_value=None):
        meta = CF_META.get(key, {})
        if base_value is None:
            base_value = value
        value = self.round_cf(value)
        return {
            'value': float(value),
            'base_value': float(base_value),
            'name': meta.get('name', key),
            'description': meta.get('description', ''),
        }

    def get_default_skills(self):
        return {skill_key: 0 for skill_key in SKILL_META}

    def normalize_skills(self):
        if not isinstance(getattr(self, 'skills', None), dict):
            self.skills = {}

        normalized = self.get_default_skills()
        for key in normalized:
            try:
                normalized[key] = max(0, int(self.skills.get(key, 0)))
            except (TypeError, ValueError):
                normalized[key] = 0
        self.skills = normalized

        try:
            self.available_skill_points = max(0, int(getattr(self, 'available_skill_points', 0)))
        except (TypeError, ValueError):
            self.available_skill_points = 0

        try:
            self.skill_points_awarded_for_level = max(1, int(getattr(self, 'skill_points_awarded_for_level', 1)))
        except (TypeError, ValueError):
            self.skill_points_awarded_for_level = 1

    def add_skill_points_for_levels(self, target_level=None):
        self.normalize_skills()
        target_level = self.level if target_level is None else target_level
        levels_to_award = max(0, target_level - self.skill_points_awarded_for_level)
        if levels_to_award <= 0:
            self.skill_points_awarded_for_level = max(self.skill_points_awarded_for_level, target_level)
            return 0

        points = levels_to_award * SKILL_POINTS_PER_LEVEL
        self.available_skill_points += points
        self.skill_points_awarded_for_level = target_level
        return points

    def increase_skill(self, skill_key, points=1, save=True):
        self.normalize_skills()
        if skill_key not in SKILL_META:
            return False, 'Неизвестное умение.'

        try:
            points = int(points)
        except (TypeError, ValueError):
            points = 0
        if points <= 0:
            return False, 'Количество баллов должно быть положительным.'
        if self.available_skill_points < points:
            return False, 'Недостаточно доступных баллов умений.'

        self.skills[skill_key] += points
        self.available_skill_points -= points
        self.update_cf()
        if save:
            self.save()

        skill_name = SKILL_META[skill_key]['name']
        return True, f'Умение "{skill_name}" увеличено на {points}.'

    def get_skill_bonus(self, skill_key):
        self.normalize_skills()
        return self.skills.get(skill_key, 0) * SKILL_CF_STEP

    def calculate_max_health(self, level=None):
        level = self.level if level is None else level
        try:
            level = max(1, int(level))
        except (TypeError, ValueError):
            level = 1
        return BASE_MAX_HEALTH + ((level - 1) // 5) * MAX_HEALTH_PER_5_LEVELS

    def update_max_health(self):
        self.max_health = self.calculate_max_health()
        return self.max_health

    def get_max_health(self):
        if not hasattr(self, 'max_health'):
            self.update_max_health()
        expected_max_health = self.calculate_max_health()
        if self.max_health != expected_max_health:
            self.max_health = expected_max_health
        return self.max_health

    def normalize_cf(self):
        """Приводит коэффициенты к формату с названием, описанием и значением."""
        if not isinstance(getattr(self, 'cf', None), dict):
            self.cf = {}

        normalized = {}
        for key, meta in CF_META.items():
            default_value = meta.get('default', 1.0)
            current_value = self.cf.get(key, default_value)
            if isinstance(current_value, dict):
                value = current_value.get('value', default_value)
                base_value = current_value.get('base_value', value)
            else:
                value = current_value
                base_value = current_value
            normalized[key] = self._make_cf_parameter(key, value, base_value)
        self.cf = normalized

    def get_cf_value(self, key, default=1.0):
        current_value = self.cf.get(key, default)
        if isinstance(current_value, dict):
            return current_value.get('value', default)
        return current_value

    def set_cf_value(self, key, value):
        self.cf[key] = self._make_cf_parameter(key, value)

    @staticmethod
    def round_cf(value):
        """Округляет коэффициент до ближайшего значения с шагом 0,05."""
        try:
            value = float(value)
        except (TypeError, ValueError):
            value = 0.0
        return math.floor((value + 1e-9) * 20 + 0.5) / 20

    def reset_cf_to_base(self):
        self.normalize_cf()
        for parameter in self.cf.values():
            parameter['value'] = parameter.get('base_value', parameter.get('value', 1.0))

    def _apply_buff_to_cf(self, buff, stacks=1):
        if buff.target_cf not in self.cf:
            return

        parameter = self.cf[buff.target_cf]
        value = parameter.get('value', 1.0) + buff.signed_value() * stacks
        parameter['value'] = max(0, self.round_cf(value))

    def get_timed_buffs(self):
        return list(self.buffs) + list(self.debuffs)

    def get_inventory_buffs(self):
        inventory_buffs = []
        merged_stackable_buffs = {}

        if not isinstance(self.items, dict):
            return inventory_buffs

        for category, items in self.items.items():
            if category not in game_data.ITEM_REGISTRY or not isinstance(items, dict):
                continue

            for item_name, count in items.items():
                if count <= 0:
                    continue

                _, item = game_data.find_registry_item(category, item_name)
                if not item:
                    continue

                buffs = item.get_buffs() if hasattr(item, 'get_buffs') else [getattr(item, 'buff', None)]
                for buff in buffs:
                    if not buff or buff.duration_minutes is not None:
                        continue

                    item_buff = buff.activate(start_time=None)
                    item_buff.start_time = None
                    item_buff.end_time = None
                    item_buff.source = item.name
                    if item_buff.stackable:
                        merge_key = (item_buff.name, item_buff.target_cf, item_buff.buff_type)
                        if merge_key in merged_stackable_buffs:
                            merged_stackable_buffs[merge_key]['total_value'] += item_buff.value * count
                            merged_stackable_buffs[merge_key]['stacks'] += count
                        else:
                            merged_stackable_buffs[merge_key] = {
                                'buff': item_buff,
                                'stacks': count,
                                'total_value': item_buff.value * count,
                            }
                    else:
                        inventory_buffs.append((item_buff, count))

        for merged_buff in merged_stackable_buffs.values():
            buff = merged_buff['buff']
            stacks = merged_buff['stacks']
            if stacks > 0:
                buff.value = merged_buff['total_value'] / stacks
            inventory_buffs.append((buff, stacks))
        return inventory_buffs

    def remove_expired_buffs(self, now=None):
        now = now or get_effective_now()
        active_buffs = [buff for buff in self.buffs if not buff.is_expired(now)]
        active_debuffs = [buff for buff in self.debuffs if not buff.is_expired(now)]
        changed = len(active_buffs) != len(self.buffs) or len(active_debuffs) != len(self.debuffs)
        self.buffs = active_buffs
        self.debuffs = active_debuffs
        return changed

    def apply_buffs_to_cf(self, save=False):
        changed = self.remove_expired_buffs()
        self.reset_cf_to_base()

        for buff in self.get_timed_buffs():
            self._apply_buff_to_cf(buff)

        for buff, stacks in self.get_inventory_buffs():
            self._apply_buff_to_cf(buff, stacks)

        for parameter in self.cf.values():
            parameter['value'] = self.round_cf(parameter.get('value', 0))

        if save or changed:
            self.save()
        return changed

    def get_all_buffs(self, positive=True, include_inventory=True):
        self.remove_expired_buffs()
        timed = self.buffs if positive else self.debuffs
        result = [(buff, 1) for buff in timed]

        if include_inventory:
            for buff, stacks in self.get_inventory_buffs():
                if buff.is_positive() == positive:
                    result.append((buff, stacks))

        return result

    def add_buff(self, buff, save=True):
        active_buff = buff.activate()
        self.remove_expired_buffs()
        target_list = self.buffs if active_buff.is_positive() else self.debuffs

        for existing_buff in target_list:
            if (
                existing_buff.name == active_buff.name
                and existing_buff.target_cf == active_buff.target_cf
                and existing_buff.buff_type == active_buff.buff_type
            ):
                if active_buff.stackable:
                    existing_buff.value += active_buff.value

                if existing_buff.end_time is None:
                    self.apply_buffs_to_cf(save=save)
                    return existing_buff

                if active_buff.end_time is None:
                    existing_buff.end_time = None
                else:
                    duration_delta = active_buff.end_time - active_buff.start_time
                    existing_buff.end_time = max(datetime.now(), existing_buff.end_time) + duration_delta

                self.apply_buffs_to_cf(save=save)
                return existing_buff

        target_list.append(active_buff)
        self.apply_buffs_to_cf(save=save)
        return active_buff

    def remove_buff(self, buff_name, positive=True):
        target_list = self.buffs if positive else self.debuffs
        initial_len = len(target_list)
        target_list[:] = [buff for buff in target_list if buff.name != buff_name]
        changed = len(target_list) != initial_len
        if changed:
            self.apply_buffs_to_cf(save=True)
        return changed

    def adjust_buff_duration(self, buff_name, minutes, positive=True):
        """Добавляет или отнимает минуты у активного временного бафа."""
        self.remove_expired_buffs()
        target_list = self.buffs if positive else self.debuffs

        for buff in target_list:
            if buff.name != buff_name or buff.end_time is None:
                continue

            buff.end_time += timedelta(minutes=minutes)
            self.apply_buffs_to_cf(save=True)
            return True

        return False

    def get_cf_description(self, key):
        parameter = self.cf.get(key, self._make_cf_parameter(key, 1.0))
        template = parameter.get('description', '')
        coins_per_100 = round(
            game_data.base_coin_bonus * self.get_cf_value('coins', 1.0), 1
        )
        exp_per_100 = round(
            game_data.base_exp_bonus * self.get_cf_value('exp', 1.0)
        )
        health_recovery_per_hour = self.get_cf_value('health_recovery', 0.0)
        return template.format(
            coins_per_100=coins_per_100,
            exp_per_100=exp_per_100,
            health_recovery_per_hour=health_recovery_per_hour,
        )

    def get_cf_parameters(self):
        self.normalize_cf()
        return [
            {
                'key': key,
                'name': parameter['name'],
                'value': parameter['value'],
                'description': self.get_cf_description(key),
            }
            for key, parameter in self.cf.items()
        ]

    # === 3. СЛУЖЕБНЫЕ МЕТОДЫ ===
    @staticmethod
    def round_money(value):
        try:
            value = float(value)
        except (TypeError, ValueError):
            value = 0
        return round(math.ceil((value - 1e-9) * 10) / 10, 1)

    @staticmethod
    def round_exp(value):
        """Округляет начисление опыта вверх до одного знака."""
        try:
            value = float(value)
        except (TypeError, ValueError):
            value = 0.0
        return round(math.ceil((value - 1e-9) * 10) / 10, 1)

    def add_exp(self, value):
        accrued = self.round_exp(value)
        self.exp += accrued
        return accrued

    def normalize_coins(self):
        self.coins = self.round_money(getattr(self, 'coins', 0))
        return self.coins

    def check_integrity(self):
        """Лечит старые сохранения"""
        self.migrate()  # Просто вызываем migrate вместо ручной проверки

    def save(self):
        self.normalize_inventory_item_names()
        self.normalize_coins()
        if _SUPPRESS_PERSISTENCE.get():
            return
        data_file = get_data_file_path()
        from nfprogress.core.sqlite import StorageOwner, StorageOwnershipRepository, Subsystem
        root = Path(data_file).parent
        if StorageOwnershipRepository(root).get_owner(Subsystem.GAME) == StorageOwner.SQLITE:
            from nfprogress.core.game_state import SQLiteGameRepository
            SQLiteGameRepository(root).write_gamer(self)
            return
        engine.atomic_pickle_save(self, data_file)

    # === МОТИВАЦИЯ И ПИСАТЕЛЬСКИЕ СЕССИИ ===
    def normalize_motivation(self):
        try:
            self.inspiration = min(MAX_INSPIRATION, max(0.0, float(self.inspiration)))
        except (TypeError, ValueError):
            self.inspiration = 0.0

        if not isinstance(self.daily_challenge, dict):
            self.daily_challenge = None
        if not isinstance(self.weekly_challenge, dict):
            self.weekly_challenge = None
        if not isinstance(self.writing_session, dict):
            self.writing_session = None
        elif self.writing_session.get('clock_source') != 'wall':
            # Старые сессии могли использовать зафиксированное время режима
            # разработчика. Начинаем их таймер заново, не теряя прогресс.
            self.writing_session['started_at'] = get_session_now()
            self.writing_session['clock_source'] = 'wall'
        if self.writing_session is not None:
            if self.writing_session.get('mode') not in WRITING_SESSION_MODES:
                self.writing_session['mode'] = 'flow'
        try:
            self.writing_session_streak = max(0, int(self.writing_session_streak))
        except (AttributeError, TypeError, ValueError):
            self.writing_session_streak = 0
        raw_history = getattr(self, 'writing_session_history', [])
        if not isinstance(raw_history, list):
            raw_history = []
        self.writing_session_history = [
            entry for entry in raw_history[-WRITING_SESSION_HISTORY_LIMIT:]
            if isinstance(entry, dict)
        ]
        for counter_field, maximum in (
                ('session_streak_shields', 3),
                ('session_grade_boosts', 1),
        ):
            try:
                value = min(maximum, max(0, int(getattr(self, counter_field))))
            except (AttributeError, TypeError, ValueError):
                value = 0
            setattr(self, counter_field, value)
        for bonus_field in (
                'writing_reward_bonus',
                'session_reward_bonus',
                'challenge_reward_bonus',
        ):
            try:
                normalized_bonus = min(
                    1.0, max(0.0, float(getattr(self, bonus_field)))
                )
            except (AttributeError, TypeError, ValueError):
                normalized_bonus = 0.0
            setattr(self, bonus_field, normalized_bonus)
        try:
            self.manuscript_reward_bonus = min(
                1.0, max(0.0, float(self.manuscript_reward_bonus))
            )
        except (TypeError, ValueError):
            self.manuscript_reward_bonus = 0.0

        raw_streak_reward_days = getattr(self, 'api_streak_reward_days', {})
        normalized_streak_reward_days = {}
        if isinstance(raw_streak_reward_days, dict):
            for key, value in raw_streak_reward_days.items():
                if not isinstance(key, str) or not key:
                    continue
                if isinstance(value, datetime):
                    value = value.date()
                elif isinstance(value, str):
                    try:
                        value = date.fromisoformat(value)
                    except ValueError:
                        continue
                if isinstance(value, date):
                    normalized_streak_reward_days[key] = value
        self.api_streak_reward_days = normalized_streak_reward_days

        today = engine.today_for_test()
        if self.daily_challenge and self.daily_challenge.get('date') != today.isoformat():
            self.daily_challenge = None
            self.daily_challenge_options = []
        if self.daily_challenge is not None:
            challenge_type = self.daily_challenge.get('type', 'symbols')
            difficulty = self.daily_challenge.get('difficulty', 'normal')
            if challenge_type not in DAILY_CHALLENGE_TYPES:
                challenge_type = 'symbols'
            if difficulty not in DAILY_CHALLENGE_DIFFICULTIES:
                difficulty = 'normal'
            try:
                target = max(1, int(self.daily_challenge.get('target', 0)))
                progress = max(0, int(self.daily_challenge.get('progress', 0)))
            except (TypeError, ValueError):
                self.daily_challenge = None
                self.daily_challenge_options = []
            else:
                self.daily_challenge['type'] = challenge_type
                self.daily_challenge['difficulty'] = difficulty
                self.daily_challenge['option_id'] = f'{challenge_type}:{difficulty}'
                self.daily_challenge['target'] = target
                self.daily_challenge['progress'] = progress
                self.daily_challenge['completed'] = bool(
                    self.daily_challenge.get('completed', False)
                )
        raw_options = getattr(self, 'daily_challenge_options', [])
        normalized_options = []
        if isinstance(raw_options, list):
            for option in raw_options:
                if (
                        not isinstance(option, dict)
                        or option.get('date') != today.isoformat()
                        or option.get('type') not in DAILY_CHALLENGE_TYPES
                        or option.get('difficulty') not in DAILY_CHALLENGE_DIFFICULTIES
                ):
                    continue
                try:
                    target = max(1, int(option.get('target', 0)))
                    progress = max(0, int(option.get('progress', 0)))
                except (TypeError, ValueError):
                    continue
                normalized = dict(option)
                normalized['target'] = target
                normalized['progress'] = progress
                normalized['completed'] = bool(option.get('completed', False))
                normalized['option_id'] = (
                    f'{option["type"]}:{option["difficulty"]}'
                )
                normalized_options.append(normalized)
        self.daily_challenge_options = normalized_options
        raw_daily_history = getattr(self, 'daily_challenge_history', [])
        self.daily_challenge_history = (
            [str(value) for value in raw_daily_history[-14:]]
            if isinstance(raw_daily_history, list) else []
        )
        try:
            self.productive_actions_since_event = max(
                0, int(getattr(self, 'productive_actions_since_event', 0))
            )
        except (TypeError, ValueError):
            self.productive_actions_since_event = 0
        pending_event = getattr(self, 'pending_creative_event', None)
        self.pending_creative_event = (
            pending_event if pending_event in CREATIVE_EVENTS else None
        )
        raw_event_history = getattr(self, 'creative_event_history', [])
        self.creative_event_history = (
            [entry for entry in raw_event_history[-20:] if isinstance(entry, dict)]
            if isinstance(raw_event_history, list) else []
        )

        week_start = today - timedelta(days=today.weekday())
        if self.weekly_challenge and self.weekly_challenge.get('week_start') != week_start.isoformat():
            self.weekly_challenge = None

        if self.specialization not in SPECIALIZATIONS:
            self.specialization = None
        raw_mastery = getattr(self, 'specialization_mastery', {})
        if not isinstance(raw_mastery, dict):
            raw_mastery = {}
        normalized_mastery = {}
        for specialization_key, value in raw_mastery.items():
            if specialization_key not in SPECIALIZATIONS:
                continue
            try:
                normalized_mastery[specialization_key] = max(0, int(value))
            except (TypeError, ValueError):
                normalized_mastery[specialization_key] = 0
        self.specialization_mastery = normalized_mastery
        raw_ready_at = getattr(self, 'specialization_ability_ready_at', {})
        if not isinstance(raw_ready_at, dict):
            raw_ready_at = {}
        normalized_ready_at = {}
        for key, value in raw_ready_at.items():
            if key not in SPECIALIZATIONS or not value:
                continue
            ready_at = as_local_naive_datetime(value)
            if ready_at is not None:
                normalized_ready_at[key] = ready_at.isoformat()
        self.specialization_ability_ready_at = normalized_ready_at
        raw_effects = getattr(self, 'specialization_ability_effects', {})
        if not isinstance(raw_effects, dict):
            raw_effects = {}
        self.specialization_ability_effects = {
            key: bool(value)
            for key, value in raw_effects.items()
            if key in SPECIALIZATIONS and bool(value)
        }
        if self.specialization_changed_at is not None:
            try:
                datetime.fromisoformat(str(self.specialization_changed_at))
            except (TypeError, ValueError):
                self.specialization_changed_at = None
        if not isinstance(self.manuscript_journeys, dict):
            self.manuscript_journeys = {}
        valid_milestones = {item['progress'] for item in MANUSCRIPT_MILESTONES}
        self.manuscript_journeys = {
            str(project_key): sorted({
                int(value) for value in values
                if isinstance(value, (int, float)) and int(value) in valid_milestones
            })
            for project_key, values in self.manuscript_journeys.items()
            if project_key and isinstance(values, (list, tuple, set))
        }
        if not isinstance(self.cabinet_relics, list):
            self.cabinet_relics = []
        self.cabinet_relics = [
            relic_key for relic_key in dict.fromkeys(self.cabinet_relics)
            if relic_key in CABINET_RELICS
        ]

    def specialization_change_days_remaining(self):
        self.normalize_motivation()
        if self.specialization is None or self.specialization_changed_at is None:
            return 0
        changed_at = datetime.fromisoformat(str(self.specialization_changed_at)).date()
        elapsed_days = (engine.today_for_test() - changed_at).days
        return max(0, SPECIALIZATION_CHANGE_COOLDOWN_DAYS - elapsed_days)

    def select_specialization(self, specialization_key, save=True):
        self.normalize_motivation()
        if self.level < SPECIALIZATION_LEVEL:
            return False, f'Специализации открываются на {SPECIALIZATION_LEVEL} уровне.'
        if specialization_key not in SPECIALIZATIONS:
            return False, 'Неизвестная специализация.'
        if specialization_key == self.specialization:
            return False, 'Эта специализация уже выбрана.'

        days_remaining = self.specialization_change_days_remaining()
        if days_remaining > 0:
            return False, f'Сменить специализацию можно через {days_remaining} дн.'

        self.specialization = specialization_key
        self.specialization_changed_at = engine.today_for_test().isoformat()
        self.specialization_mastery.setdefault(specialization_key, 0)
        if save:
            self.save()
        return True, f'Выбрана специализация «{SPECIALIZATIONS[specialization_key]["name"]}».'

    def get_specialization_reward_multiplier(self, reward_type, symbols=0, intention=None):
        specialization = self.specialization
        bonus = self.get_specialization_bonus(specialization)
        if specialization == 'marathoner' and reward_type == 'writing' and symbols >= 3000:
            return 1 + bonus
        if specialization == 'ritualist' and reward_type == 'session':
            return 1 + bonus
        if specialization == 'finisher' and reward_type == 'completion':
            return 1 + bonus
        if specialization == 'explorer' and reward_type == 'challenge':
            return 1 + bonus
        if (
                specialization == 'editor'
                and reward_type == 'session'
                and intention == 'Отредактировать текст'
        ):
            return 1 + bonus
        return 1.0

    def specialization_mastery_rank(self, specialization_key=None):
        specialization_key = specialization_key or self.specialization
        if specialization_key not in SPECIALIZATIONS:
            return 0
        mastery = getattr(self, 'specialization_mastery', {})
        xp = mastery.get(specialization_key, 0) if isinstance(mastery, dict) else 0
        return sum(xp >= threshold for threshold in SPECIALIZATION_MASTERY_THRESHOLDS)

    def get_specialization_bonus(self, specialization_key=None):
        specialization_key = specialization_key or self.specialization
        meta = SPECIALIZATIONS.get(specialization_key)
        if meta is None:
            return 0.0
        rank = max(1, self.specialization_mastery_rank(specialization_key))
        return meta['base_bonus'] + (rank - 1) * meta['mastery_step']

    def add_specialization_mastery(self, specialization_key, amount=1):
        if specialization_key not in SPECIALIZATIONS:
            return None
        try:
            amount = max(0, int(amount))
        except (TypeError, ValueError):
            return None
        if amount == 0:
            return None

        mastery = self.specialization_mastery
        old_rank = self.specialization_mastery_rank(specialization_key)
        mastery[specialization_key] = mastery.get(specialization_key, 0) + amount
        new_rank = self.specialization_mastery_rank(specialization_key)
        if new_rank <= old_rank:
            return None
        return (
            f'Мастерство специализации «{SPECIALIZATIONS[specialization_key]["name"]}» '
            f'повышено до ранга {new_rank}!'
        )

    def specialization_ability_remaining_seconds(self, specialization_key=None):
        specialization_key = specialization_key or self.specialization
        if specialization_key not in SPECIALIZATION_ABILITIES:
            return 0
        ready_at = self.specialization_ability_ready_at.get(specialization_key)
        if not ready_at:
            return 0
        ready_at = as_local_naive_datetime(ready_at)
        if ready_at is None:
            self.specialization_ability_ready_at.pop(specialization_key, None)
            return 0
        current_time = as_local_naive_datetime(get_effective_now())
        if current_time is None:
            current_time = datetime.now()
        return max(0, math.ceil((ready_at - current_time).total_seconds()))

    def activate_specialization_ability(self, save=True):
        self.normalize_motivation()
        specialization = self.specialization
        ability = SPECIALIZATION_ABILITIES.get(specialization)
        if ability is None:
            return False, 'Сначала выберите специализацию.'
        remaining = self.specialization_ability_remaining_seconds(specialization)
        if remaining > 0:
            return False, 'Активное умение специализации ещё восстанавливается.'
        if self.specialization_ability_effects.get(specialization):
            return False, 'Эффект активного умения уже ожидает применения.'

        if specialization == 'explorer':
            challenge = self.weekly_challenge
            if not challenge or challenge.get('completed'):
                return False, 'Нет активного недельного испытания для замены.'
            keys = tuple(WEEKLY_CHALLENGES)
            current_key = challenge.get('key')
            current_index = keys.index(current_key) if current_key in keys else -1
            challenge.update({
                'key': keys[(current_index + 1) % len(keys)],
                'progress': 0,
                'writing_days': [],
                'completed': False,
            })
        else:
            self.specialization_ability_effects[specialization] = True

        current_time = as_local_naive_datetime(get_effective_now())
        if current_time is None:
            current_time = datetime.now()
        ready_at = current_time + timedelta(
            hours=SPECIALIZATION_ABILITY_COOLDOWN_HOURS
        )
        self.specialization_ability_ready_at[specialization] = ready_at.isoformat()
        if save:
            self.save()
        return True, f'Активировано умение «{ability["name"]}».'

    def _specialization_ability_multiplier(
            self, reward_type, symbols=0, intention=None
    ):
        specialization = self.specialization
        if not self.specialization_ability_effects.get(specialization):
            return 1.0
        applicable = (
            specialization == 'marathoner'
            and reward_type == 'writing'
            and symbols >= 3000
        ) or (
            specialization == 'ritualist'
            and reward_type == 'session'
        ) or (
            specialization == 'finisher'
            and reward_type == 'milestone'
        ) or (
            specialization == 'editor'
            and reward_type == 'session'
            and intention == 'Отредактировать текст'
        )
        if not applicable:
            return 1.0
        return 1 + SPECIALIZATION_ABILITIES[specialization]['bonus']

    def _consume_specialization_ability(self, specialization_key=None):
        specialization_key = specialization_key or self.specialization
        self.specialization_ability_effects.pop(specialization_key, None)

    def advance_manuscript_journey(self, project_key, progress):
        self.normalize_motivation()
        if not project_key:
            return []
        try:
            progress = min(100.0, max(0.0, float(progress)))
        except (TypeError, ValueError):
            return []

        project_key = str(project_key)
        received = self.manuscript_journeys.setdefault(project_key, [])
        messages = []
        milestones_reached = 0
        ability_multiplier = self._specialization_ability_multiplier('milestone')
        reward_multiplier = (
            (1 + self.manuscript_reward_bonus)
            * ability_multiplier
            * (1 + self.get_cabinet_bonus('milestone'))
        )
        for milestone in MANUSCRIPT_MILESTONES:
            threshold = milestone['progress']
            if threshold > progress or threshold in received:
                continue
            received.append(threshold)
            milestones_reached += 1
            coins = self.set_coins(
                milestone['coins'] * self.calculate_inflation() * reward_multiplier,
                save=False,
            )
            exp = self.add_exp(milestone['exp'] * reward_multiplier)
            inspiration = self.add_inspiration(milestone['inspiration'])
            messages.append(
                f'Рубеж рукописи «{milestone["name"]}» достигнут! '
                f'Получено {coins} монет, {exp} опыта и '
                f'{inspiration:g} вдохновения.'
            )
        received.sort()
        if messages:
            self.manuscript_reward_bonus = 0.0
            if ability_multiplier > 1:
                self._consume_specialization_ability('finisher')
        if self.specialization == 'finisher' and milestones_reached:
            mastery_message = self.add_specialization_mastery(
                'finisher', milestones_reached
            )
            if mastery_message:
                messages.append(mastery_message)
        messages.extend(self.unlock_cabinet_relics())
        return messages

    def unlock_cabinet_relics(self):
        messages = []
        for relic_key, relic in CABINET_RELICS.items():
            if relic_key in self.cabinet_relics:
                continue
            qualifying_projects, _ = self.cabinet_relic_progress(relic_key)
            if qualifying_projects < relic['required_projects']:
                continue
            self.cabinet_relics.append(relic_key)
            messages.append(f'Новая реликвия в кабинете: «{relic["name"]}».')
        return messages

    def cabinet_relic_progress(self, relic_key):
        relic = CABINET_RELICS.get(relic_key)
        if relic is None:
            return 0, 0
        qualifying_projects = sum(
            relic['required_progress'] in milestones
            or any(value >= relic['required_progress'] for value in milestones)
            for milestones in self.manuscript_journeys.values()
        )
        return min(qualifying_projects, relic['required_projects']), relic['required_projects']

    def get_unlocked_cabinet_sets(self):
        unlocked = set(self.cabinet_relics)
        return [
            set_key for set_key, meta in CABINET_SETS.items()
            if set(meta['relics']).issubset(unlocked)
        ]

    def get_cabinet_bonus(self, reward_type):
        bonus = sum(
            relic['bonus']
            for relic_key, relic in CABINET_RELICS.items()
            if relic_key in self.cabinet_relics
            and relic['effect_type'] == reward_type
        )
        for set_key in self.get_unlocked_cabinet_sets():
            cabinet_set = CABINET_SETS[set_key]
            if (
                    cabinet_set['effect_type'] == reward_type
                    or (
                        cabinet_set['effect_type'] == 'all_rewards'
                        and reward_type != 'inspiration'
                    )
            ):
                bonus += cabinet_set['bonus']
        return round(bonus, 4)

    def add_inspiration(self, value):
        try:
            value = max(0.0, float(value))
        except (TypeError, ValueError):
            return 0.0
        value *= 1 + self.get_cabinet_bonus('inspiration')
        old_value = self.inspiration
        self.inspiration = min(MAX_INSPIRATION, round(self.inspiration + value, 2))
        return round(self.inspiration - old_value, 2)

    def get_manuscript_journey_status(self, progress):
        try:
            progress = min(100.0, max(0.0, float(progress)))
        except (TypeError, ValueError):
            progress = 0.0
        reached = None
        upcoming = None
        for milestone in MANUSCRIPT_MILESTONES:
            if milestone['progress'] <= progress:
                reached = milestone
            elif upcoming is None:
                upcoming = milestone
        return reached, upcoming

    def rename_manuscript_journey(self, old_key, new_key, save=True):
        self.normalize_motivation()
        if not old_key or not new_key or old_key == new_key:
            return False
        old_values = self.manuscript_journeys.pop(str(old_key), [])
        if not old_values:
            return False
        current_values = self.manuscript_journeys.setdefault(str(new_key), [])
        current_values[:] = sorted(set(current_values) | set(old_values))
        if save:
            self.save()
        return True

    @staticmethod
    def calculate_adaptive_daily_target(data=None):
        """Возвращает посильную цель: 80% среднего результата за продуктивные дни."""
        data = engine.load_data() if data is None else data
        today = engine.today_for_test()
        earliest_day = today - timedelta(days=14)
        symbols_by_day = {}

        def collect_notes(project):
            stages = getattr(project, 'stages', []) if getattr(project, 'enable_stages', False) else []
            if stages:
                for stage in stages:
                    collect_notes(stage)
                return
            for note in getattr(project, 'notes', []):
                note_day = note.get_date_create()
                added = note.get_added_symbols()
                if earliest_day <= note_day < today and added > 0:
                    symbols_by_day[note_day] = symbols_by_day.get(note_day, 0) + added

        for project in data.get('projects', {}).values():
            collect_notes(project)

        if not symbols_by_day:
            return 1000
        average = sum(symbols_by_day.values()) / len(symbols_by_day)
        target = min(5000, max(500, average * 0.8))
        return int(round(target / 100) * 100)

    def ensure_daily_challenge(self, target=None):
        self.normalize_motivation()
        if self.daily_challenge is None:
            self.daily_challenge_options = self.generate_daily_challenge_options(target)
            self.daily_challenge = dict(self.daily_challenge_options[0])
        elif not self.daily_challenge_options:
            generated = self.generate_daily_challenge_options(target)
            current_id = self.daily_challenge.get('option_id')
            self.daily_challenge_options = [dict(self.daily_challenge)] + [
                option for option in generated
                if option.get('option_id') != current_id
            ][:2]
        return self.daily_challenge

    def generate_daily_challenge_options(self, target=None):
        today = engine.today_for_test()
        base_target = int(target or self.calculate_adaptive_daily_target())
        type_keys = tuple(DAILY_CHALLENGE_TYPES)
        shift = today.toordinal() % len(type_keys)
        type_keys = type_keys[shift:] + type_keys[:shift]
        difficulty_keys = ('normal', 'easy', 'hard')
        options = []
        for challenge_type, difficulty_key in zip(type_keys, difficulty_keys):
            difficulty = DAILY_CHALLENGE_DIFFICULTIES[difficulty_key]
            if challenge_type == 'symbols':
                challenge_target = max(
                    100,
                    int(round(base_target * difficulty['target_multiplier'] / 100) * 100),
                )
                reward_coins = challenge_target / 20
                reward_exp = challenge_target * 0.5
            elif challenge_type == 'sessions':
                challenge_target = {'easy': 1, 'normal': 2, 'hard': 3}[difficulty_key]
                reward_coins = 120 * challenge_target
                reward_exp = 500 * challenge_target
            else:
                challenge_target = {'easy': 1, 'normal': 1, 'hard': 2}[difficulty_key]
                reward_coins = 150 * challenge_target
                reward_exp = 600 * challenge_target
            options.append({
                'date': today.isoformat(),
                'option_id': f'{challenge_type}:{difficulty_key}',
                'type': challenge_type,
                'difficulty': difficulty_key,
                'target': challenge_target,
                'progress': 0,
                'completed': False,
                'reward_coins': reward_coins * difficulty['reward_multiplier'],
                'reward_exp': reward_exp * difficulty['reward_multiplier'],
            })
        return options

    def select_daily_challenge_option(self, option_index, free=False, save=True):
        self.normalize_motivation()
        daily = self.ensure_daily_challenge()
        if daily.get('completed'):
            return False, 'Выполненную цель дня заменить нельзя.'
        try:
            option = self.daily_challenge_options[int(option_index)]
        except (IndexError, TypeError, ValueError):
            return False, 'Неизвестный вариант цели дня.'
        if option.get('option_id') == daily.get('option_id'):
            return False, 'Этот вариант цели дня уже выбран.'
        if not free and self.inspiration < DAILY_CHALLENGE_CHANGE_COST:
            return False, 'Недостаточно вдохновения для смены цели дня.'
        if not free:
            self.inspiration -= DAILY_CHALLENGE_CHANGE_COST
        self.daily_challenge = dict(option)
        if save:
            self.save()
        return True, 'Выбрана новая цель дня.'

    def _advance_daily_challenge(
            self, symbols=0, successful_session=False, session_intention=None
    ):
        daily = self.ensure_daily_challenge()
        if daily.get('completed'):
            return []
        challenge_type = daily.get('type', 'symbols')
        if challenge_type == 'symbols' and symbols > 0:
            daily['progress'] += symbols
        elif challenge_type == 'sessions' and successful_session:
            daily['progress'] += 1
        elif (
                challenge_type == 'editing'
                and successful_session
                and session_intention == 'Отредактировать текст'
        ):
            daily['progress'] += 1
        if daily['progress'] < daily['target']:
            return []

        daily['completed'] = True
        self.daily_challenge_history.append(daily.get('option_id', challenge_type))
        self.daily_challenge_history = self.daily_challenge_history[-14:]
        reward_multiplier = self._consume_challenge_reward_multiplier()
        reward_coins = daily.get('reward_coins', daily['target'] / 20)
        reward_exp = daily.get('reward_exp', daily['target'] * 0.5)
        coins = self.set_coins(
            reward_coins * self.calculate_inflation() * reward_multiplier,
            save=False,
        )
        exp = self.add_exp(reward_exp * reward_multiplier)
        inspiration = self.add_inspiration(10)
        messages = [
            f'Цель дня выполнена! Получено {coins} монет, {exp} опыта и '
            f'{inspiration:g} вдохновения.'
        ]
        if self.specialization == 'explorer':
            mastery_message = self.add_specialization_mastery('explorer')
            if mastery_message:
                messages.append(mastery_message)
        return messages

    def _maybe_trigger_creative_event(self, symbols):
        if symbols < 1000 or self.pending_creative_event is not None:
            return None
        self.productive_actions_since_event += 1
        if self.productive_actions_since_event < CREATIVE_EVENT_PRODUCTIVE_INTERVAL:
            return None
        self.productive_actions_since_event = 0
        event_keys = tuple(CREATIVE_EVENTS)
        event_key = event_keys[len(self.creative_event_history) % len(event_keys)]
        self.pending_creative_event = event_key
        event = CREATIVE_EVENTS[event_key]
        return f'Творческое событие «{event["name"]}» ожидает вашего решения.'

    def resolve_creative_event(self, choice, save=True):
        self.normalize_motivation()
        event_key = self.pending_creative_event
        event = CREATIVE_EVENTS.get(event_key)
        if event is None:
            return False, 'Нет творческого события, ожидающего решения.'
        if choice not in ('safe', 'risk'):
            return False, 'Неизвестный выбор творческого события.'

        success = choice == 'safe' or random.random() < 0.55
        effect = event['safe'] if choice == 'safe' else event['risk']
        if success:
            effect_type, value = effect[0], effect[1]
        else:
            effect_type, value = effect[2], effect[3]

        if effect_type == 'inspiration':
            gained = self.add_inspiration(value)
            result = f'Получено {gained:g} вдохновения.'
        elif effect_type == 'inspiration_loss':
            lost = min(self.inspiration, float(value))
            self.inspiration -= lost
            result = f'Потеряно {lost:g} вдохновения.'
        elif effect_type == 'writing_bonus':
            self.writing_reward_bonus = min(1.0, self.writing_reward_bonus + value)
            result = f'Бонус к следующей записи: +{value * 100:g}%.'
        elif effect_type == 'coins':
            coins = self.set_coins(value * self.calculate_inflation(), save=False)
            result = f'Получено {coins} монет.'
        else:
            result = 'Риск не принёс награды.'

        self.creative_event_history.append({
            'event': event_key,
            'choice': choice,
            'success': success,
            'resolved_at': get_effective_now().isoformat(),
        })
        self.creative_event_history = self.creative_event_history[-20:]
        self.pending_creative_event = None
        if save:
            self.save()
        return True, f'Событие «{event["name"]}» завершено. {result}'

    def activate_inspiration_ability(self, ability_key, save=True):
        self.normalize_motivation()
        ability = INSPIRATION_ABILITIES.get(ability_key)
        if ability is None:
            return False, 'Неизвестная способность вдохновения.'

        bonus_field = ability['bonus_field']
        if getattr(self, bonus_field) > 0:
            return False, 'Этот эффект вдохновения уже активен.'
        if self.inspiration < ability['cost']:
            return False, 'Недостаточно вдохновения для этой способности.'

        self.inspiration -= ability['cost']
        setattr(self, bonus_field, ability['bonus'])
        if save:
            self.save()
        return (
            True,
            f'Активирована способность «{ability["name"]}». '
            f'Потрачено {ability["cost"]} вдохновения.',
        )

    def _consume_challenge_reward_multiplier(self):
        reward_multiplier = (
            (1 + self.challenge_reward_bonus)
            * self.get_specialization_reward_multiplier('challenge')
            * (1 + self.get_cabinet_bonus('challenge'))
        )
        self.challenge_reward_bonus = 0.0
        return reward_multiplier

    def select_weekly_challenge(self, challenge_key, save=True):
        self.normalize_motivation()
        if challenge_key not in WEEKLY_CHALLENGES:
            return False, 'Неизвестное недельное испытание.'
        if self.weekly_challenge is not None:
            return False, 'Недельное испытание уже выбрано.'

        today = engine.today_for_test()
        week_start = today - timedelta(days=today.weekday())
        self.weekly_challenge = {
            'key': challenge_key,
            'week_start': week_start.isoformat(),
            'progress': 0,
            'writing_days': [],
            'completed': False,
        }
        if save:
            self.save()
        return True, f'Начато недельное испытание «{WEEKLY_CHALLENGES[challenge_key]["name"]}».'

    def start_writing_session(
            self, duration_minutes, target_symbols, intention, mode_key='flow', save=True
    ):
        self.normalize_motivation()
        if self.writing_session is not None:
            return False, 'Сначала завершите текущую писательскую сессию.'
        try:
            duration_minutes = int(duration_minutes)
            target_symbols = int(target_symbols)
        except (TypeError, ValueError):
            return False, 'Некорректные параметры писательской сессии.'
        if duration_minutes not in (15, 25, 45, 60) or target_symbols <= 0:
            return False, 'Выберите длительность и положительную цель сессии.'
        if mode_key not in WRITING_SESSION_MODES:
            return False, 'Неизвестный режим писательской сессии.'
        if mode_key == 'sprint' and duration_minutes != 15:
            return False, 'Спринт рассчитан только на 15 минут.'
        if mode_key == 'deep' and duration_minutes not in (45, 60):
            return False, 'Глубокая работа рассчитана на 45 или 60 минут.'
        if mode_key == 'editing' and intention != 'Отредактировать текст':
            return False, 'Редакторский проход требует намерения отредактировать текст.'

        self.writing_session = {
            'started_at': get_session_now(),
            'clock_source': 'wall',
            'duration_minutes': duration_minutes,
            'target_symbols': target_symbols,
            'progress': 0,
            'intention': str(intention),
            'mode': mode_key,
        }
        if save:
            self.save()
        return True, 'Писательская сессия начата.'

    def writing_session_remaining_seconds(self):
        self.normalize_motivation()
        if self.writing_session is None:
            return 0
        started_at = self.writing_session.get('started_at')
        if not isinstance(started_at, datetime):
            return 0
        duration = self.writing_session.get('duration_minutes', 0) * 60
        elapsed = (get_session_now() - started_at).total_seconds()
        return max(0, math.ceil(duration - elapsed))

    @staticmethod
    def writing_session_grade(progress, target):
        try:
            ratio = max(0.0, float(progress)) / max(1.0, float(target))
        except (TypeError, ValueError):
            ratio = 0.0
        for grade_key, grade_name, threshold, reward_multiplier in WRITING_SESSION_GRADES:
            if ratio >= threshold:
                return grade_key, grade_name, reward_multiplier
        return 'failed', 'Цель не достигнута', 0.0

    def _record_writing_session_result(
            self, session, grade_key, successful, coins=0, exp=0
    ):
        self.writing_session_history.append({
            'finished_at': get_effective_now().isoformat(),
            'mode': session.get('mode', 'flow'),
            'intention': session.get('intention', ''),
            'duration_minutes': session.get('duration_minutes', 0),
            'target_symbols': session.get('target_symbols', 0),
            'progress': session.get('progress', 0),
            'grade': grade_key,
            'successful': bool(successful),
            'coins': coins,
            'exp': exp,
        })
        self.writing_session_history = self.writing_session_history[
            -WRITING_SESSION_HISTORY_LIMIT:
        ]

    def finish_writing_session(self, save=True):
        self.normalize_motivation()
        if self.writing_session is None:
            return False, 'Нет активной писательской сессии.'

        session = self.writing_session
        remaining_seconds = self.writing_session_remaining_seconds()
        grade_key, grade_name, grade_multiplier = self.writing_session_grade(
            session.get('progress', 0), session.get('target_symbols', 1)
        )
        successful = grade_key != 'failed'
        grade_boosted = False
        if successful and self.session_grade_boosts > 0 and grade_key != 'gold':
            grade_order = {
                'bronze': ('silver', 'Серебро', 1.15),
                'silver': ('gold', 'Золото', 1.30),
            }
            grade_key, grade_name, grade_multiplier = grade_order[grade_key]
            self.session_grade_boosts -= 1
            grade_boosted = True
        self.writing_session = None
        if not successful:
            protection_message = None
            protected = (
                self.specialization == 'ritualist'
                and self.specialization_ability_effects.get('ritualist')
            )
            if protected:
                self._consume_specialization_ability('ritualist')
                protection_message = 'Сила ритуала сохранила серию успешных сессий.'
            elif self.session_streak_shields > 0:
                self.session_streak_shields -= 1
                protected = True
                protection_message = 'Нить ритуала сохранила серию успешных сессий.'
            else:
                self.writing_session_streak = 0
            self._record_writing_session_result(session, grade_key, False)
            if save:
                self.save()
            message = 'Сессия завершена. Цель не достигнута — штрафа нет.'
            if protected:
                message += f'\n{protection_message}'
            return False, message

        inspiration = self.add_inspiration(10)
        mode = WRITING_SESSION_MODES.get(
            session.get('mode', 'flow'), WRITING_SESSION_MODES['flow']
        )
        ability_multiplier = self._specialization_ability_multiplier(
            'session', intention=session.get('intention')
        )
        self.writing_session_streak += 1
        streak_multiplier = 1 + min(self.writing_session_streak - 1, 5) * 0.03
        early_finish_multiplier = (
            1.10 if remaining_seconds > 0 and grade_key in ('silver', 'gold') else 1.0
        )
        reward_multiplier = (
            (1 + self.session_reward_bonus)
            * self.get_specialization_reward_multiplier(
                'session', intention=session.get('intention')
            )
            * ability_multiplier
            * (1 + self.get_cabinet_bonus('session'))
            * (1 + mode['reward_bonus'])
            * grade_multiplier
            * streak_multiplier
            * early_finish_multiplier
        )
        self.session_reward_bonus = 0.0
        if ability_multiplier > 1:
            self._consume_specialization_ability()
        coins = self.set_coins(25 * self.calculate_inflation() * reward_multiplier, save=False)
        exp = self.add_exp(250 * reward_multiplier)
        mastery_message = None
        if self.specialization == 'ritualist':
            mastery_message = self.add_specialization_mastery('ritualist')
        elif (
                self.specialization == 'editor'
                and session.get('intention') == 'Отредактировать текст'
        ):
            mastery_message = self.add_specialization_mastery('editor')
        daily_messages = self._advance_daily_challenge(
            successful_session=True,
            session_intention=session.get('intention'),
        )
        weekly_message = self._advance_weekly_challenge(
            0,
            successful_session=True,
            session_intention=session.get('intention'),
        )
        self._record_writing_session_result(
            session, grade_key, True, coins=coins, exp=exp
        )
        if save:
            self.save()
        message = (
            f'Сессия завершена! Результат: {grade_name}. '
            f'Стрик: {self.writing_session_streak}. '
            f'Получено {coins} монет, {exp} опыта и {inspiration:g} вдохновения.'
        )
        if early_finish_multiplier > 1:
            message += '\nБонус за досрочный высокий результат: +10%.'
        if grade_boosted:
            message += '\nМедаль качества повысила результат сессии.'
        if weekly_message:
            message += f'\n{weekly_message}'
        if daily_messages:
            message += '\n' + '\n'.join(daily_messages)
        if mastery_message:
            message += f'\n{mastery_message}'
        return True, message

    def cancel_writing_session(self, save=True):
        self.normalize_motivation()
        if self.writing_session is None:
            return False, 'Нет активной писательской сессии.'
        self.writing_session = None
        if save:
            self.save()
        return True, 'Писательская сессия отменена без штрафа.'

    def _advance_weekly_challenge(
            self, symbols, successful_session=False, session_intention=None
    ):
        if not self.weekly_challenge or self.weekly_challenge.get('completed'):
            return None
        challenge = self.weekly_challenge
        challenge_key = challenge.get('key')
        meta = WEEKLY_CHALLENGES.get(challenge_key)
        if meta is None:
            return None

        if challenge_key == 'symbols':
            challenge['progress'] += symbols
        elif challenge_key == 'days' and symbols > 0:
            today = engine.today_for_test().isoformat()
            days = challenge.setdefault('writing_days', [])
            if today not in days:
                days.append(today)
            challenge['progress'] = len(days)
        elif challenge_key == 'sessions' and successful_session:
            challenge['progress'] += 1
        elif (
                challenge_key == 'editing'
                and successful_session
                and session_intention == 'Отредактировать текст'
        ):
            challenge['progress'] += 1

        if challenge['progress'] < meta['target']:
            return None
        challenge['completed'] = True
        reward_multiplier = self._consume_challenge_reward_multiplier()
        coins = self.set_coins(
            meta['reward_coins'] * self.calculate_inflation() * reward_multiplier,
            save=False,
        )
        exp = self.add_exp(meta['reward_exp'] * reward_multiplier)
        inspiration = self.add_inspiration(20)
        message = f'Недельное испытание завершено! Получено {coins} монет, {exp} опыта и {inspiration:g} вдохновения.'
        if self.specialization == 'explorer':
            mastery_message = self.add_specialization_mastery('explorer')
            if mastery_message:
                message += f'\n{mastery_message}'
        return message

    def record_motivation_progress(self, symbols):
        self.normalize_motivation()
        symbols = max(0, int(symbols))
        if symbols <= 0:
            return []

        messages = []
        self.add_inspiration(symbols / INSPIRATION_SYMBOL_STEP)
        if self.writing_session is not None:
            self.writing_session['progress'] = self.writing_session.get('progress', 0) + symbols

        messages.extend(self._advance_daily_challenge(symbols=symbols))

        weekly_message = self._advance_weekly_challenge(symbols)
        if weekly_message:
            messages.append(weekly_message)
        event_message = self._maybe_trigger_creative_event(symbols)
        if event_message:
            messages.append(event_message)
        return messages

    def record_editing_progress(self, symbols):
        """Count removed characters only for an active editing session."""
        self.normalize_motivation()
        if (
                self.writing_session is None
                or self.writing_session.get('intention') != 'Отредактировать текст'
        ):
            return 0
        try:
            processed_symbols = abs(int(symbols))
        except (TypeError, ValueError):
            return 0
        if processed_symbols <= 0:
            return 0
        self.writing_session['progress'] = (
            self.writing_session.get('progress', 0) + processed_symbols
        )
        return processed_symbols

    # === 4. ИГРОВАЯ ЛОГИКА ===
    def give_symbol_bonus(self, symbols, project_key=None, project_progress=None):
        self.normalize_motivation()
        reward_multiplier = (
            (1 + self.inspiration / MAX_INSPIRATION * 0.1)
            * (1 + self.writing_reward_bonus)
            * self.get_specialization_reward_multiplier('writing', symbols=symbols)
            * self._specialization_ability_multiplier('writing', symbols=symbols)
            * (1 + self.get_cabinet_bonus('writing'))
        )
        exp_cf = self.get_cf_value('exp', 1.0)
        exps = self.add_exp(
            symbols / 100 * game_data.base_exp_bonus * exp_cf * reward_multiplier
        )
        self.save()
        coins_cf = self.get_cf_value('coins', 1.0)
        coins = symbols / 100 * game_data.base_coin_bonus * coins_cf * reward_multiplier
        coins = self.set_coins(coins)
        if symbols > 0:
            self.writing_reward_bonus = 0.0
        if (
                self.specialization == 'marathoner'
                and symbols >= 3000
                and self.specialization_ability_effects.get('marathoner')
        ):
            self._consume_specialization_ability('marathoner')
        mastery_message = None
        if self.specialization == 'marathoner' and symbols >= 3000:
            mastery_message = self.add_specialization_mastery('marathoner')
        motivation_messages = self.record_motivation_progress(symbols)
        journey_messages = self.advance_manuscript_journey(project_key, project_progress)
        self.save()
        message = f'Получено {coins} монет\nПолучено {exps} опыта'
        extra_messages = [*motivation_messages, *journey_messages]
        if mastery_message:
            extra_messages.append(mastery_message)
        if extra_messages:
            message += '\n' + '\n'.join(extra_messages)
        return message

    def give_streak_bonus(self, status, streak_type, streak_len=1, project_name=None):
        if (
                streak_type == 'Local'
                and isinstance(status, str)
                and 'Complete' in status.split()
                and project_name in self.complete_bonus_projects
        ):
            return None

        if streak_type == 'Global' and isinstance(status, str) and 'Lose' in status.split():
            runtime_data = _RUNTIME_DATA.get()
            data = runtime_data if runtime_data is not None else engine.load_data()
            refresh_result = engine.refresh_project_streak_statuses(data)
            if refresh_result.get('freeze_changed'):
                if runtime_data is not None:
                    # The current Gamer is already authoritative in this
                    # transaction; the old reload only repaired a pickle-side
                    # freeze inventory race.
                    self.normalize_inventory_item_names()
                else:
                    # Preserve the legacy oracle behavior for migration and
                    # non-SQLite callers that still use the paired PKLs.
                    self.items = load_game().items
            refreshed_status = engine.global_streak_status(data)
            if isinstance(refreshed_status, str) and 'Lose' not in refreshed_status.split():
                status = refreshed_status
                streak_len = engine.streak_length(data.get('global_streaks', []))

        st = status.split()
        cf_coins = self.get_cf_value('coins')
        cf_exp = self.get_cf_value('exp')
        msg = None

        # Комбинированный статус для глобального стрика (урон + бонус)
        if 'Lose' in st and 'Start' in st and streak_type == 'Global':
            today = engine.today_for_test()
            days = int(st[1]) if len(st) > 1 and st[1].isdigit() else 1
            if self.last_lose_global_streak_damage != today:
                damage = days * 5
                self.damage(damage)
                self.last_lose_global_streak_damage = today
                self.last_lose_global_streak_damage_amount = damage
            else:
                damage = days * 5  # для отображения
            bonus = self.set_coins(10 * cf_coins * self.calculate_inflation())
            msg = (f'🥺 СТРИК ПОТЕРЯН\n'
                   f'Урон за потерю глобального стрика: {damage}❤️\n'
                   f'🔥 Новый стрик начат! Бонус: {bonus} монет')

        # Комбинированный статус для локального стрика (только бонус за старт)
        elif 'Lose' in st and 'Start' in st and streak_type == 'Local':
            bonus = self.set_coins(10 * cf_coins * self.calculate_inflation())
            msg = f'Получен бонус {bonus} монет за старт стрика в проекте (после потери).'

        # Обычный старт (без потери)
        elif 'Start' in st and 'Lose' not in st:
            if streak_type == 'Local':
                bonus = self.set_coins(50 * cf_coins)
                msg = f'Получен бонус {bonus} монет за старт стрика в проекте.'
            else:
                bonus = self.set_coins(50 * cf_coins)
                msg = f'Получен бонус {bonus} монет за старт глобального стрика.'

        # Продолжение стрика
        elif 'Go' in st:
            if streak_type == 'Local':
                coin_bonus = self.set_coins(10 * cf_coins * streak_len * self.calculate_inflation())
                exp_bonus = self.round_exp(100 * streak_len * cf_exp)
                msg = f'Получен бонус {coin_bonus} монет и {exp_bonus} оп. за продление стрика в проекте.'
            else:
                coin_bonus = self.set_coins(10 * cf_coins * streak_len * self.calculate_inflation())
                exp_bonus = self.round_exp(1000 * streak_len * cf_exp)
                msg = f'Получен бонус {coin_bonus} монет и {exp_bonus} оп. за продление глобального стрика.'
            self.add_exp(exp_bonus)

        # Завершение стрика (только локальный)
        elif 'Complete' in st:
            coin_bonus = self.set_coins(25 * cf_coins * streak_len * self.calculate_inflation())
            exp_bonus = self.round_exp(5000 * streak_len * cf_exp)
            msg = (f'СТРИК В ПРОЕКТЕ ЗАВЕРШЕН!'
                   f'\nВы были в цели {streak_len} д. подряд!'
                   f'\nВы получили награду: {coin_bonus} монет и {exp_bonus} опыта!')
            self.add_exp(exp_bonus)

        # Чистая потеря (только глобальный)
        elif 'Lose' in st and streak_type == 'Global':
            today = engine.today_for_test()
            if self.last_lose_global_streak_damage != today:
                days = 1
                for part in st:
                    if part.isdigit():
                        days = int(part)
                        break
                damage = days * 5
                self.damage(damage)
                self.last_lose_global_streak_damage = today
                self.last_lose_global_streak_damage_amount = damage
                msg = (f'🥺 СТРИК ПОТЕРЯН\n'
                       f'Урон за потерю глобального стрика: {damage}❤️')

        self.save()
        return msg

    def give_complete_bonus(self, project_status, project_total, project_name=None, bonus_multiplier=1.0):
        if project_name and not self.mark_complete_bonus_received(project_name):
            return None

        bonus_multiplier *= (
            self.get_specialization_reward_multiplier('completion')
            * (1 + self.get_cabinet_bonus('completion'))
        )
        cf_total = round(project_total / 1000 + 0.5)  # обычное деление, не целочисленное
        cf_coins = self.get_cf_value('coins')
        cf_exp = self.get_cf_value('exp')

        coin_bonus = self.set_coins(100 * cf_total * cf_coins * bonus_multiplier)
        exp_bonus = self.round_exp(10000 * cf_total * cf_exp * bonus_multiplier)

        self.add_exp(exp_bonus)
        msg = f'Вы получили награду {coin_bonus} монет и {exp_bonus} оп.'

        if self.specialization == 'finisher':
            mastery_message = self.add_specialization_mastery('finisher', 3)
            if mastery_message:
                msg += f'\n{mastery_message}'

        self.save()
        return msg

    def mark_complete_bonus_received(self, project_name):
        """Отмечает проект, за завершение которого уже была выдана награда."""
        if not project_name or project_name in self.complete_bonus_projects:
            return False
        self.complete_bonus_projects.append(project_name)
        return True

    def sync_quests(self):
        """Добавляет новые квесты из каталога, сохраняя прогресс существующих."""
        if not isinstance(getattr(self, 'quests', None), list):
            self.quests = []

        existing_quests = {}
        for quest in self.quests:
            if isinstance(quest, Quest):
                quest.normalize()
                existing_quests[quest.quest_id] = quest

        try:
            import gama_quests
            quest_catalog = gama_quests.get_quests()
        except ImportError:
            quest_catalog = []

        synced_quests = []
        for catalog_quest in quest_catalog:
            saved_quest = existing_quests.get(catalog_quest.quest_id)
            if saved_quest:
                saved_quest.name = catalog_quest.name
                saved_quest.description = catalog_quest.description
                saved_quest.reward_coins = catalog_quest.reward_coins
                saved_quest.reward_exp = catalog_quest.reward_exp
                saved_quest.reward_items = catalog_quest.reward_items
                saved_quest.reward_buffs = catalog_quest.reward_buffs
                saved_quest.level = catalog_quest.level
                saved_quest.quest_func = catalog_quest.quest_func
                synced_quests.append(saved_quest)
            else:
                synced_quests.append(catalog_quest)

        self.quests = synced_quests
        self.refresh_available_quests()
        return self.quests

    def migrate_completed_quest_item_rewards(self):
        """Выдает предметы за квесты, завершенные до появления предметных наград."""
        granted_items = []
        compensated_coins = 0
        processed_quests = False
        for quest in self.quests:
            if (quest.status != Quest.COMPLETED
                    or getattr(quest, 'reward_items_received', None) is True):
                continue
            quest_items, quest_compensation = quest.give_reward_items(
                self, skip_existing_awards=True,
            )
            granted_items.extend(quest_items)
            compensated_coins += quest_compensation
            quest.reward_items_received = True
            processed_quests = True
        compensated_coins = self.round_money(compensated_coins)
        if granted_items or compensated_coins:
            self.pending_quest_item_migration_notification = self.format_quest_item_migration_notification(
                granted_items, compensated_coins,
            )
        return processed_quests

    @staticmethod
    def format_quest_item_migration_notification(granted_items, compensated_coins):
        item_count = sum(count for _, count in granted_items)
        item_lines = []
        if item_count:
            item_lines.append(f'Получено предметов: {item_count} шт.')
        if compensated_coins:
            item_lines.append(f'Денежная компенсация: {compensated_coins:g} монет.')
        return 'Бонус за старые квесты в связи с обновлением:\n' + '\n'.join(
            f'- {item}' for item in item_lines
        )

    def consume_quest_item_migration_notification(self):
        message = getattr(self, 'pending_quest_item_migration_notification', None)
        self.pending_quest_item_migration_notification = None
        return message

    def refresh_available_quests(self):
        """Проверяет условия доступности квестов, сейчас базовое условие - уровень."""
        changed = False
        for quest in self.quests:
            quest.normalize()
            if quest.status == Quest.AVAILABLE and self.level < quest.level:
                continue
            if quest.status not in (Quest.AVAILABLE, Quest.ACTIVE, Quest.COMPLETED):
                quest.status = Quest.AVAILABLE
                changed = True
        return changed

    def update_quests(self, save=True):
        """Синхронизирует квесты и завершает активные, если условия выполнены."""
        self.sync_quests()
        messages = []
        changed = self.refresh_available_quests()

        for quest in self.quests:
            if quest.check_conditions(self):
                message = quest.complete(self)
                if message:
                    messages.append(message)
                    changed = True

        if changed and save:
            self.save()
        return messages

    def get_quests_by_status(self, status):
        self.sync_quests()
        return [quest for quest in self.quests if quest.status == status and self.level >= quest.level]

    def get_quest(self, quest_id):
        self.sync_quests()
        for quest in self.quests:
            if quest.quest_id == quest_id:
                return quest
        return None

    def start_quest(self, quest_id):
        quest = self.get_quest(quest_id)
        if not quest:
            return False, 'Квест не найден.'

        ok, message = quest.start(self)
        if ok:
            self.save()
        return ok, message

    def abandon_quest(self, quest_id):
        quest = self.get_quest(quest_id)
        if not quest or quest.status != Quest.ACTIVE:
            return False, 'Активный квест не найден.'

        quest.status = Quest.AVAILABLE
        quest.start_date = None
        quest.end_date = None
        self.save()
        return True, f'Квест "{quest.name}" возвращен в доступные.'

    def get_items(self):
        return self.items

    def set_items(self, items):
        self.items = items
        self.normalize_inventory_item_names()

    def remove_coins(self, removed, process_bank_events=True, save=True):
        removed = self.round_money(removed)
        self.coins = self.round_money(self.coins - removed)
        self.calculate_inflation()
        if process_bank_events:
            self.process_bank_events(save=False)
        if save:
            self.save()
        return removed

    def get_coins(self):
        return self.normalize_coins()

    def set_coins(self, coins, process_bank_events=True, save=True):
        coins = self.round_money(coins)
        self.coins = self.round_money(self.coins + coins)
        self.calculate_inflation()
        if process_bank_events:
            self.process_bank_events(save=False)
        if save:
            self.save()
        return coins

    def process_bank_events(self, save=True):
        bank_account = getattr(self, 'bank_account', None)
        if not bank_account:
            return []
        messages = bank_account.process_daily_events(self, auto_pay=True, notify=True, save=False)
        if messages and save:
            self.save()
        return messages

    def update_cf(self):
        """Обновляет коэффициенты согласно уровню и вложенным баллам умений."""
        self.normalize_skills()
        self.set_cf_value('coins', game_data.cf_coins[self.level] + self.get_skill_bonus('profitability'))
        self.set_cf_value('exp', game_data.cf_exp[self.level] + self.get_skill_bonus('productivity'))
        self.set_cf_value('health_recovery', self.get_skill_bonus('endurance'))
        self.apply_buffs_to_cf(save=False)

    def recover_health_by_time(self, now=None, save=True):
        now = now or get_effective_now()
        if not isinstance(getattr(self, 'last_health_recovery_at', None), datetime):
            self.last_health_recovery_at = now

        if now < self.last_health_recovery_at:
            self.last_health_recovery_at = now
            if save:
                self.save()
            return None

        recovery_per_hour = self.get_cf_value('health_recovery', 0.0)
        if recovery_per_hour <= 0 or self.health <= 0:
            self.last_health_recovery_at = now
            return None

        max_health = self.get_max_health()
        if self.health >= max_health:
            self.health = max_health
            self.last_health_recovery_at = now
            return None

        elapsed_seconds = max(0, (now - self.last_health_recovery_at).total_seconds())
        recovered = elapsed_seconds / 3600 * recovery_per_hour
        if recovered < 0.1:
            return None

        old_health = self.health
        self.health = round(min(max_health, self.health + recovered), 1)
        self.last_health_recovery_at = now
        restored = round(self.health - old_health, 1)
        if save:
            self.save()

        if restored <= 0:
            return None
        return f'Здоровье восстановлено на {restored:g}. Текущее здоровье: {self.health:g}/{max_health}.'

    def level_up(self):
        msg = False
        while self.level < len(game_data.levels) - 1 and self.exp >= game_data.levels[self.level]:
            new_level = self.level + 1
            coins_bonus = game_data.lvl_coins_bonus[self.level] * self.calculate_inflation()

            self.level = new_level
            self.exp = self.exp - game_data.levels[self.level - 1]
            self.update_max_health()
            self.health = self.get_max_health()
            self.last_health_recovery_at = get_effective_now()
            coins_bonus = self.set_coins(coins_bonus, process_bank_events=False, save=False)

            awarded_skill_points = self.add_skill_points_for_levels(self.level)
            self.update_cf()

            msg = f'ПОЛУЧЕН НОВЫЙ {new_level} УРОВЕНЬ! Ваш бонус: {coins_bonus} монет'
            if awarded_skill_points:
                msg += f'\nПолучено {awarded_skill_points} балла умений.'

        if msg:
            self.save()
        return msg

    def check_health(self):
        if self.health > 0:
            return True

        # Ищем любое зелье в инвентаре по категории и названию предмета.
        has_potion = any(
            count > 0 and 'зелье' in str(item_name).casefold()
            for item_name, count in self.items.get('Зелья', {}).items()
        )

        if has_potion:
            # Для простоты в критической ситуации даем шанс восстановиться вручную
            return False
        elif self.get_coins() >= 100:
            choice = input('1 - Купить и применить зелье восстановления (100 монет): ')
            if choice == '1':
                self.remove_coins(100, process_bank_events=False, save=False)
                self.update_max_health()
                self.health = self.get_max_health()
                self.save()
                return True


        self.reset()
        return False

    def damage(self, damage):
        self.health -= damage
        self.last_health_recovery_at = get_effective_now()
        self.save()
        return (f'Вы потеряли {damage} ед. здоровья'
                f'У вас осталось {self.health} ед. здоровья')

    def reset(self):
        self.__init__()
        self.save()

    def check_loan_penalty(self):
        pass

    def migrate_legacy_award_names(self):
        """Переносит старые ключи наград с эмодзи на ключи без эмодзи."""
        if not isinstance(self.items, dict):
            return False

        awards = self.items.setdefault('Награды', {})
        if not isinstance(awards, dict):
            self.items['Награды'] = {}
            return True

        legacy_awards = {
            '👑 Корона Первой Эпохи': 'Корона Первой Эпохи',
            '💎 Перо Миллионера': 'Перо Миллионера',
        }

        changed = False
        for old_name, new_name in legacy_awards.items():
            old_count = awards.pop(old_name, 0)
            if old_count > 0:
                awards[new_name] = awards.get(new_name, 0) + old_count
                changed = True

        return changed

    def normalize_inventory_item_names(self):
        """Сводит ключи инвентаря к ключам реестра, чтобы алиасы не считались разными предметами."""
        if not isinstance(self.items, dict):
            return False

        changed = False
        for category, category_items in list(self.items.items()):
            if not isinstance(category_items, dict):
                self.items[category] = {}
                changed = True
                continue

            normalized_items = {}
            for item_name, count in category_items.items():
                registry_key, _ = game_data.find_registry_item(category, item_name)
                normalized_name = registry_key or item_name
                normalized_items[normalized_name] = normalized_items.get(normalized_name, 0) + count
                if normalized_name != item_name:
                    changed = True

            if normalized_items != category_items:
                self.items[category] = normalized_items
                changed = True

        return changed

    def normalize_buff_names(self):
        """Добавляет эмодзи новым названиям уже сохранённых активных бафов."""
        legacy_names = {
            'Квестовая специализация: опыт': '⭐️ Квестовая специализация: опыт',
            'Квестовая специализация: монеты': '⭐️ Квестовая специализация: монеты',
            'Квестовая специализация: восстановление': '⭐️ Квестовая специализация: восстановление',
            'Опыт миллионера': '👑 Опыт миллионера',
            'Удача миллионера': '💎 Удача миллионера',
            'Бустер опыта': '🧪⚡️ Бустер опыта',
            'Супер бустер опыта': '🧪⚡️ Супер бустер опыта',
            'Минибустер прибыли': '🧪⚡️ Минибустер прибыли',
        }
        changed = False
        for buff in [*self.buffs, *self.debuffs]:
            normalized_name = legacy_names.get(buff.name)
            if normalized_name:
                buff.name = normalized_name
                changed = True
        return changed

    def migrate(self):
        """Проверяет наличие всех атрибутов и добавляет недостающие"""
        had_skill_award_marker = hasattr(self, 'skill_points_awarded_for_level')
        had_max_health_marker = hasattr(self, 'max_health')
        had_motivation_marker = hasattr(self, 'inspiration')
        had_specialization_marker = hasattr(self, 'specialization')
        had_journey_marker = hasattr(self, 'manuscript_journeys')
        had_cabinet_marker = hasattr(self, 'cabinet_relics')
        complete_bonus_projects_migrated = not hasattr(self, 'complete_bonus_projects')
        defaults = {
            'level': 1,
            'exp': 0,
            'coins': 0,
            'health': 100,
            'max_health': self.calculate_max_health(),
            'cf': {
                'coins': self._make_cf_parameter('coins', 1.0),
                'exp': self._make_cf_parameter('exp', 1.0),
                'health_recovery': self._make_cf_parameter('health_recovery', 0.0),
            },
            'skills': self.get_default_skills(),
            'available_skill_points': 0,
            'skill_points_awarded_for_level': 1,
            'last_health_recovery_at': get_effective_now(),
            'items': {'Предметы': {},'Зелья': {},'Награды': {},'Парфюм': {}},
            'custom_awards': [],
            'custom_awards_inventory': {},
            'notifications': {'new': [], 'read': []},
            'bank_account': None,
            'last_lose_global_streak_damage': None,
            'last_bonus_dates': {},
            'api_streak_reward_days': {},
            'complete_bonus_projects': [],
            'inflation': 1,
            'buffs': [],
            'debuffs': [],
            'quests': [],
            'inspiration': 0,
            'daily_challenge': None,
            'daily_challenge_options': [],
            'daily_challenge_history': [],
            'weekly_challenge': None,
            'productive_actions_since_event': 0,
            'pending_creative_event': None,
            'creative_event_history': [],
            'writing_session': None,
            'writing_session_streak': 0,
            'writing_session_history': [],
            'session_streak_shields': 0,
            'session_grade_boosts': 0,
            'writing_reward_bonus': 0.0,
            'session_reward_bonus': 0.0,
            'challenge_reward_bonus': 0.0,
            'manuscript_reward_bonus': 0.0,
            'specialization': None,
            'specialization_changed_at': None,
            'specialization_mastery': {},
            'specialization_ability_ready_at': {},
            'specialization_ability_effects': {},
            'manuscript_journeys': {},
            'cabinet_relics': [],
            'pending_quest_item_migration_notification': None,
        }

        for attr, default_value in defaults.items():
            # Проверка флага финансовой реформы
            if not hasattr(self, 'economy_rebalanced_v1'):
                setattr(self, 'economy_rebalanced_v1', False)
            if not hasattr(self, attr):
                setattr(self, attr, default_value)
            elif attr == 'cf' and not isinstance(getattr(self, attr), dict):
                setattr(self, attr, {
                    'coins': self._make_cf_parameter('coins', 1.0),
                    'exp': self._make_cf_parameter('exp', 1.0),
                })
            elif attr == 'items' and not isinstance(getattr(self, attr), dict):
                setattr(self, attr, {})
            elif attr == 'notifications':
                notifications = getattr(self, attr)
                if not isinstance(notifications, dict):
                    setattr(self, attr, {'new': [], 'read': []})
                else:
                    # Убеждаемся, что оба ключа существуют
                    if 'new' not in notifications:
                        notifications['new'] = []
                    if 'read' not in notifications:
                        notifications['read'] = []
            elif attr in ('buffs', 'debuffs') and not isinstance(getattr(self, attr), list):
                setattr(self, attr, [])
            elif attr == 'quests' and not isinstance(getattr(self, attr), list):
                setattr(self, attr, [])
            elif attr == 'complete_bonus_projects':
                projects = getattr(self, attr)
                normalized_projects = (
                    [name for name in projects if isinstance(name, str)]
                    if isinstance(projects, list) else []
                )
                if projects != normalized_projects:
                    setattr(self, attr, normalized_projects)
                    complete_bonus_projects_migrated = True
            elif attr == 'last_health_recovery_at' and not isinstance(getattr(self, attr), datetime):
                setattr(self, attr, get_effective_now())

        if not self.economy_rebalanced_v1:
            # Считаем, сколько сейчас стоит зелье для игрока
            current_potion_cost = int(200 * self.calculate_inflation())

            # Определяем потолок адекватного богатства (например, стоимость 15 зелий)
            sane_balance_limit = current_potion_cost * 10

            if self.get_coins() > sane_balance_limit:
                # Создаем раздел 'Награды', если его еще нет в инвентаре
                if 'Награды' not in self.items:
                    self.items['Награды'] = {}
                # Если игрок сверхбогат, даем ему памятный предмет ветерана
                self.items['Награды']['👑 Корона Первой Эпохи'] = 1

                # Если у него больше миллиона монет, даем еще один уникальный статус
                if self.get_coins() >= 1000000:
                    self.items['Награды']['💎 Перо Миллионера'] = 1

                # Срезаем баланс до адекватного лимита
                self.coins = self.round_money(sane_balance_limit)

            # Отмечаем, что реформа пройдена
            self.economy_rebalanced_v1 = True
            self.save()

        # Задаем структуру инвентаря
        if self.items == {}:
            self.items = {'Предметы': {},'Зелья': {},'Награды': {},'Парфюм': {}}
        for _category in ('Предметы', 'Зелья', 'Награды', 'Парфюм'):
            self.items.setdefault(_category, {})
        migrated_awards = self.migrate_legacy_award_names()
        migrated_inventory = self.normalize_inventory_item_names()
        migrated_buff_names = self.normalize_buff_names()
        self.normalize_coins()
        old_health = getattr(self, 'health', 0)
        old_max_health = getattr(self, 'max_health', None)
        self.update_max_health()
        max_health_migrated = not had_max_health_marker or old_max_health != self.max_health
        if max_health_migrated and old_health == BASE_MAX_HEALTH and self.max_health > BASE_MAX_HEALTH:
            self.health = self.max_health

        # Особая обработка для bank_account
        self.normalize_cf()
        self.normalize_skills()
        self.normalize_motivation()
        skill_points_migrated = False
        if not had_skill_award_marker:
            self.skill_points_awarded_for_level = 1
            skill_points_migrated = bool(self.add_skill_points_for_levels(self.level))
        self.update_cf()
        self.sync_quests()
        migrated_quest_item_rewards = self.migrate_completed_quest_item_rewards()
        if self.bank_account is None:
            self.bank_account = game_data.BankAccount()
        else:
            self.bank_account.normalize()

        if (migrated_awards or migrated_inventory or migrated_buff_names
                or skill_points_migrated or max_health_migrated or complete_bonus_projects_migrated
                or migrated_quest_item_rewards or not had_motivation_marker
                or not had_specialization_marker or not had_journey_marker
                or not had_cabinet_marker):
            self.save()

    def calculate_inflation(self):
        """
        Считает инфляцию цен в зависимости от уровня игрока.
        Например, +15% к базовой цене за каждый уровень после первого.
        Уровень 1: множитель 1.0 (базовые цены)
        Уровень 2: множитель 1.15
        Уровень 10: множитель 2.35
        """
        self.inflation = 1.0 + (self.level - 1) * 0.15
        return self.inflation

def load_game():
    """Загружает данные игрока из кроссплатформенной директории"""
    data_file = get_data_file_path()

    try:
        # 1. Открываем и считываем файл.
        # Как только этот блок завершается, Python автоматически закрывает файл.
        with open(data_file, 'rb') as f:
            gamer = pickle.load(f)
    except (FileNotFoundError, EOFError):
        # Если файла нет или он пуст, создаем нового игрока
        return Gamer()

    # 2. Файл гарантированно закрыт, блокировка Windows снята.
    # Теперь вызов migrate() и все внутренние вызовы save() отработают без ошибок.
    gamer.migrate()
    return gamer
