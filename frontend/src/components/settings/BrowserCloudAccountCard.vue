<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { ApiError } from '@/api/client'
import type { CloudProjectsResponse } from '@/api/cloudProjects'
import { confirmationFor } from '@/api/legalDocuments'
import { KeyNotProvisionedError } from '@/auth/keyContext'
import { StaleAuthContextError } from '@/auth/userAuth'
import { CryptoError } from '@/crypto'
import { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import { BrowserCloudReadError, type BrowserCloudProjectRead as CloudProjectMetadata } from '@/platform/web/cloudRead'
import { BrowserPersistenceError } from '@/platform/web/persistence'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'
import BrowserEncryptedWorkspace from './BrowserEncryptedWorkspace.vue'
import BrowserCloudProjectRead from './BrowserCloudProjectRead.vue'

type Copy = { unlocked: string; badPassword: string; writerBusy: string; storageUnavailable: string; legalRequired: string; acceptRevision: string; acceptCurrent: string; stale: string; onlineOnly: string; cloudProjects: string; refreshProjects: string; projectCount: string; noCloudProjects: string; projectIdsOnly: string; localWorkspace: string; connectionFailed: string; openCloudProject: string }
const copy: Record<SupportedLanguage, Copy> = {
  ru: { unlocked: 'Ключ шифрования разблокирован.', badPassword: 'Не удалось разблокировать ключ. Проверьте пароль шифрования.', writerBusy: 'Другая вкладка использует защищённое хранилище. Закройте её и повторите попытку.', storageUnavailable: 'Защищённое хранилище браузера недоступно.', legalRequired: 'Требуется принять действующие условия', acceptRevision: 'Я ознакомился(-ась) и принимаю эту редакцию', acceptCurrent: 'Принять действующие условия', stale: 'Редакция условий изменилась. Ознакомьтесь с новым текстом и подтвердите его заново.', onlineOnly: 'Web-версия требует подключения к серверу. Сервер хранит постоянные пользовательские данные; E2EE выполняется в браузере.', cloudProjects: 'Облачные проекты worta', refreshProjects: 'Обновить список', projectCount: 'Проектов на сервере: {count} из {limit}', noCloudProjects: 'На сервере пока нет облачных проектов.', projectIdsOnly: 'Сервер вернул только идентификаторы. После разблокировки можно открыть поддерживаемые заметки для чтения.', localWorkspace: 'Открыть локальные тестовые проекты (не синхронизированы)', connectionFailed: 'Не удалось получить список с сервера. Проверьте подключение и повторите попытку.', openCloudProject: 'Открыть облачный проект' },
  en: { unlocked: 'Encryption key unlocked.', badPassword: 'Could not unlock the key. Check the encryption password.', writerBusy: 'Another tab is using secure storage. Close it and try again.', storageUnavailable: 'Secure browser storage is unavailable.', legalRequired: 'Current terms must be accepted', acceptRevision: 'I have read and accept this version', acceptCurrent: 'Accept current terms', stale: 'The terms changed. Review the new text and confirm it again.', onlineOnly: 'Web requires a server connection. The server stores persistent user data; E2EE runs in this browser.', cloudProjects: 'worta cloud projects', refreshProjects: 'Refresh list', projectCount: 'Server projects: {count} of {limit}', noCloudProjects: 'There are no cloud projects on the server yet.', projectIdsOnly: 'The server returns identifiers only. Unlock to read supported notes.', localWorkspace: 'Open local test projects (not synced)', connectionFailed: 'Could not load the server list. Check your connection and try again.', openCloudProject: 'Open cloud project' },
  es: { unlocked: 'Clave de cifrado desbloqueada.', badPassword: 'No se pudo desbloquear la clave. Comprueba la contraseña de cifrado.', writerBusy: 'Otra pestaña está usando el almacenamiento seguro. Ciérrala e inténtalo de nuevo.', storageUnavailable: 'El almacenamiento seguro del navegador no está disponible.', legalRequired: 'Debes aceptar las condiciones vigentes', acceptRevision: 'He leído y acepto esta versión', acceptCurrent: 'Aceptar las condiciones vigentes', stale: 'Las condiciones han cambiado. Revisa el texto nuevo y confírmalo otra vez.', onlineOnly: 'La Web requiere conexión al servidor. El servidor guarda los datos; E2EE se ejecuta en este navegador.', cloudProjects: 'Proyectos en la nube de worta', refreshProjects: 'Actualizar lista', projectCount: 'Proyectos en el servidor: {count} de {limit}', noCloudProjects: 'Aún no hay proyectos en la nube.', projectIdsOnly: 'El servidor solo devuelve identificadores. Desbloquea para leer las notas compatibles.', localWorkspace: 'Abrir proyectos locales de prueba (sin sincronizar)', connectionFailed: 'No se pudo cargar la lista del servidor. Comprueba la conexión e inténtalo de nuevo.', openCloudProject: 'Abrir proyecto en la nube' },
  de: { unlocked: 'Verschlüsselungsschlüssel entsperrt.', badPassword: 'Der Schlüssel konnte nicht entsperrt werden. Prüfe das Verschlüsselungspasswort.', writerBusy: 'Ein anderer Tab verwendet den sicheren Speicher. Schließe ihn und versuche es erneut.', storageUnavailable: 'Sicherer Browserspeicher ist nicht verfügbar.', legalRequired: 'Die aktuellen Bedingungen müssen akzeptiert werden', acceptRevision: 'Ich habe diese Fassung gelesen und akzeptiere sie', acceptCurrent: 'Aktuelle Bedingungen akzeptieren', stale: 'Die Bedingungen wurden geändert. Lies den neuen Text und bestätige ihn erneut.', onlineOnly: 'Web benötigt eine Serververbindung. Der Server speichert Nutzerdaten; E2EE läuft im Browser.', cloudProjects: 'worta-Cloudprojekte', refreshProjects: 'Liste aktualisieren', projectCount: 'Serverprojekte: {count} von {limit}', noCloudProjects: 'Es gibt noch keine Cloudprojekte auf dem Server.', projectIdsOnly: 'Der Server liefert nur Kennungen. Entsperre den Schlüssel, um unterstützte Notizen zu lesen.', localWorkspace: 'Lokale Testprojekte öffnen (nicht synchronisiert)', connectionFailed: 'Die Serverliste konnte nicht geladen werden. Verbindung prüfen und erneut versuchen.', openCloudProject: 'Cloudprojekt öffnen' },
  fr: { unlocked: 'Clé de chiffrement déverrouillée.', badPassword: 'Impossible de déverrouiller la clé. Vérifiez le mot de passe de chiffrement.', writerBusy: 'Un autre onglet utilise le stockage sécurisé. Fermez-le et réessayez.', storageUnavailable: 'Le stockage sécurisé du navigateur est indisponible.', legalRequired: 'Les conditions en vigueur doivent être acceptées', acceptRevision: 'J’ai lu et j’accepte cette version', acceptCurrent: 'Accepter les conditions en vigueur', stale: 'Les conditions ont changé. Consultez le nouveau texte et confirmez-le à nouveau.', onlineOnly: 'Le Web nécessite une connexion au serveur. Le serveur conserve les données ; E2EE est exécuté dans ce navigateur.', cloudProjects: 'Projets cloud worta', refreshProjects: 'Actualiser la liste', projectCount: 'Projets serveur : {count} sur {limit}', noCloudProjects: 'Aucun projet cloud sur le serveur pour le moment.', projectIdsOnly: 'Le serveur ne renvoie que des identifiants. Déverrouillez pour lire les notes prises en charge.', localWorkspace: 'Ouvrir les projets de test locaux (non synchronisés)', connectionFailed: 'Impossible de charger la liste du serveur. Vérifiez la connexion et réessayez.', openCloudProject: 'Ouvrir le projet cloud' },
  pt_BR: { unlocked: 'Chave de criptografia desbloqueada.', badPassword: 'Não foi possível desbloquear a chave. Verifique a senha de criptografia.', writerBusy: 'Outra aba está usando o armazenamento seguro. Feche-a e tente novamente.', storageUnavailable: 'O armazenamento seguro do navegador não está disponível.', legalRequired: 'É necessário aceitar os termos atuais', acceptRevision: 'Li e aceito esta versão', acceptCurrent: 'Aceitar os termos atuais', stale: 'Os termos mudaram. Leia o novo texto e confirme novamente.', onlineOnly: 'A Web exige conexão com o servidor. O servidor mantém os dados; E2EE é executado neste navegador.', cloudProjects: 'Projetos na nuvem worta', refreshProjects: 'Atualizar lista', projectCount: 'Projetos no servidor: {count} de {limit}', noCloudProjects: 'Ainda não há projetos na nuvem no servidor.', projectIdsOnly: 'O servidor retorna apenas identificadores. Desbloqueie para ler as notas compatíveis.', localWorkspace: 'Abrir projetos locais de teste (não sincronizados)', connectionFailed: 'Não foi possível carregar a lista do servidor. Verifique a conexão e tente novamente.', openCloudProject: 'Abrir projeto na nuvem' },
}

const creationCopy = {
  ru: { title: 'Создать облачный проект', name: 'Название проекта', saving: 'Сохраняем проект…', saved: 'Облачный проект создан.', failed: 'Не удалось подтвердить создание. Проверьте подключение и повторите с тем же названием.', invalid: 'Название должно занимать не более 512 байт.', limit: 'Достигнут лимит облачных проектов.', unavailable: 'Создание проектов пока недоступно для этого аккаунта.' },
  en: { title: 'Create cloud project', name: 'Project name', saving: 'Saving project…', saved: 'Cloud project created.', failed: 'Could not confirm creation. Check your connection and retry with the same name.', invalid: 'Enter a name of at most 512 bytes.', limit: 'The cloud project limit has been reached.', unavailable: 'Project creation is not yet available for this account.' },
  es: { title: 'Crear proyecto en la nube', name: 'Nombre del proyecto', saving: 'Guardando proyecto…', saved: 'Proyecto en la nube creado.', failed: 'No se pudo confirmar la creación. Comprueba tu conexión y vuelve a intentarlo con el mismo nombre.', invalid: 'Introduce un nombre de hasta 512 bytes.', limit: 'Se ha alcanzado el límite de proyectos en la nube.', unavailable: 'Esta cuenta aún no puede crear proyectos.' },
  de: { title: 'Cloudprojekt erstellen', name: 'Projektname', saving: 'Projekt wird gespeichert…', saved: 'Cloudprojekt erstellt.', failed: 'Die Erstellung konnte nicht bestätigt werden. Prüfe die Verbindung und versuche es erneut mit demselben Namen.', invalid: 'Gib einen Namen mit höchstens 512 Byte ein.', limit: 'Das Limit für Cloudprojekte ist erreicht.', unavailable: 'Für dieses Konto ist das Erstellen von Projekten noch nicht verfügbar.' },
  fr: { title: 'Créer un projet cloud', name: 'Nom du projet', saving: 'Enregistrement du projet…', saved: 'Projet cloud créé.', failed: 'Impossible de confirmer la création. Vérifiez votre connexion et réessayez avec le même nom.', invalid: 'Saisissez un nom de 512 octets maximum.', limit: 'La limite de projets cloud est atteinte.', unavailable: 'La création de projets n’est pas encore disponible pour ce compte.' },
  pt_BR: { title: 'Criar projeto na nuvem', name: 'Nome do projeto', saving: 'Salvando projeto…', saved: 'Projeto na nuvem criado.', failed: 'Não foi possível confirmar a criação. Verifique a conexão e tente novamente com o mesmo nome.', invalid: 'Digite um nome de até 512 bytes.', limit: 'O limite de projetos na nuvem foi atingido.', unavailable: 'A criação de projetos ainda não está disponível para esta conta.' },
} satisfies Record<SupportedLanguage, Record<'title' | 'name' | 'saving' | 'saved' | 'failed' | 'invalid' | 'limit' | 'unavailable', string>>

const setupCopy = {
  ru: { action: 'Настроить облачную синхронизацию', hint: 'Это действие доступно для пустого аккаунта без других устройств.', busy: 'Проверяем облачную синхронизацию…', failed: 'Не удалось настроить синхронизацию. Повторите попытку.', used: 'Первоначальная настройка недоступна: в аккаунте уже есть проекты, история или другое устройство.' },
  en: { action: 'Set up cloud sync', hint: 'Available for an empty account with no other devices.', busy: 'Checking cloud sync…', failed: 'Could not set up sync. Try again.', used: 'Initial setup is unavailable because the account already has projects, history, or another device.' },
  es: { action: 'Configurar sincronización en la nube', hint: 'Disponible para una cuenta vacía sin otros dispositivos.', busy: 'Comprobando la sincronización…', failed: 'No se pudo configurar la sincronización. Inténtalo de nuevo.', used: 'La cuenta ya tiene proyectos, historial u otro dispositivo.' },
  de: { action: 'Cloud-Synchronisierung einrichten', hint: 'Für ein leeres Konto ohne weitere Geräte verfügbar.', busy: 'Cloud-Synchronisierung wird geprüft…', failed: 'Synchronisierung konnte nicht eingerichtet werden. Versuche es erneut.', used: 'Das Konto hat bereits Projekte, einen Verlauf oder ein weiteres Gerät.' },
  fr: { action: 'Configurer la synchronisation cloud', hint: 'Disponible pour un compte vide sans autre appareil.', busy: 'Vérification de la synchronisation…', failed: 'Impossible de configurer la synchronisation. Réessayez.', used: 'Le compte contient déjà des projets, un historique ou un autre appareil.' },
  pt_BR: { action: 'Configurar sincronização na nuvem', hint: 'Disponível para uma conta vazia sem outros dispositivos.', busy: 'Verificando sincronização…', failed: 'Não foi possível configurar a sincronização. Tente novamente.', used: 'A conta já contém projetos, histórico ou outro dispositivo.' },
} satisfies Record<SupportedLanguage, Record<'action' | 'hint' | 'busy' | 'failed' | 'used', string>>

const locale = useLocaleStore()
const t = locale.translate
const words = computed(() => copy[locale.language])
const createWords = computed(() => creationCopy[locale.language])
const setupWords = computed(() => setupCopy[locale.language])
const props = defineProps<{ session?: BrowserCloudAccountSession; workspacePage?: boolean }>()
const emit = defineEmits<{ statusChange: [status: BrowserCloudAccountSession['status']] }>()
const session = props.session ?? new BrowserCloudAccountSession()
const status = ref(session.status)
const username = ref('')
const accountPasswordInput = ref<HTMLInputElement | null>(null)
const encryptionPasswordInput = ref<HTMLInputElement | null>(null)
const connectedUsername = ref<string | null>(null)
const busy = ref(false)
const error = ref<string | null>(null)
const needsProvisioning = ref(false)
const provisioningBusy = ref(false)
const provisioningError = ref<string | null>(null)
const recoveryKey = ref<string | null>(null)
const provisioningPasswordInput = ref<HTMLInputElement | null>(null)
const provisioningConfirmInput = ref<HTMLInputElement | null>(null)
const workspaceOpen = ref(false)
const workspace = ref<{ canLeave: () => boolean } | null>(null)
const legalConfirmed = ref<Record<string, boolean>>({})
const cloudProjects = ref<CloudProjectsResponse | null>(null)
const cloudProjectMetadata = ref<Record<string, CloudProjectMetadata | null>>({})
const cloudProjectMetadataLoading = ref<Record<string, boolean>>({})
const cloudProjectMetadataError = ref<Record<string, string>>({})
const selectedCloudProject = ref<string | null>(null)
const cloudCatalogOpen = ref(false)
const cloudCatalogBusy = ref(false)
const cloudCatalogError = ref<string | null>(null)
const cloudProjectName = ref('')
const cloudCreateBusy = ref(false)
const cloudCreateSaved = ref(false)
const cloudCreateError = ref<string | null>(null)
let cloudCreateController: AbortController | null = null
const syncMode = ref<1 | 2 | 3 | null>(null)
const syncBusy = ref(false)
const syncError = ref<string | null>(null)
let syncController: AbortController | null = null
let cloudCatalogGeneration = 0
let cloudCatalogController: AbortController | null = null

function refresh(): void {
  status.value = session.status
  connectedUsername.value = session.username
}

function clearCloudCatalog(): void {
  selectedCloudProject.value = null
  cloudCatalogGeneration += 1
  cloudCatalogController?.abort()
  cloudCatalogController = null
  cloudProjects.value = null
  cloudProjectMetadata.value = {}
  cloudProjectMetadataLoading.value = {}
  cloudProjectMetadataError.value = {}
  cloudCatalogError.value = null
  cloudCatalogBusy.value = false
  cloudCatalogOpen.value = false
}

async function loadCloudProjectMetadata(ids: string[], controller: AbortController, generation: number): Promise<void> {
  if (typeof session.readCloudProject !== 'function') return
  const loading = { ...cloudProjectMetadataLoading.value }
  for (const id of ids) loading[id] = true
  cloudProjectMetadataLoading.value = loading
  await Promise.all(ids.map(async id => {
    try {
      const value = await session.readCloudProject(id, controller.signal)
      if (generation !== cloudCatalogGeneration || controller.signal.aborted) return
      cloudProjectMetadata.value = { ...cloudProjectMetadata.value, [id]: value }
      const nextError = { ...cloudProjectMetadataError.value }
      delete nextError[id]
      cloudProjectMetadataError.value = nextError
    } catch (cause) {
      if (generation !== cloudCatalogGeneration || controller.signal.aborted) return
      cloudProjectMetadata.value = { ...cloudProjectMetadata.value, [id]: null }
      cloudProjectMetadataError.value = { ...cloudProjectMetadataError.value, [id]: cause instanceof BrowserCloudReadError || cause instanceof BrowserPersistenceError ? cause.code : 'connection_failed' }
    } finally {
      if (generation === cloudCatalogGeneration && !controller.signal.aborted) {
        cloudProjectMetadataLoading.value = { ...cloudProjectMetadataLoading.value, [id]: false }
      }
    }
  }))
}

async function loadCloudCatalog(): Promise<void> {
  selectedCloudProject.value = null
  cloudCatalogController?.abort()
  const controller = new AbortController(), generation = ++cloudCatalogGeneration
  cloudCatalogController = controller
  cloudCatalogBusy.value = true
  cloudCatalogError.value = null
  try {
    const result = await session.listCloudProjects(controller.signal)
    if (generation !== cloudCatalogGeneration || !['authenticated', 'unlocked'].includes(session.status)) return
    cloudProjects.value = result
    if (session.status === 'unlocked') void loadCloudProjectMetadata(result.cloud_project_ids, controller, generation)
  } catch (cause) {
    if (generation !== cloudCatalogGeneration || controller.signal.aborted) return
    if (cause instanceof StaleAuthContextError) {
      refresh()
      clearCloudCatalog()
      return
    }
    if (cause instanceof ApiError && cause.status === 401) {
      refresh()
      clearCloudCatalog()
      return
    }
    cloudCatalogError.value = words.value.connectionFailed
  } finally {
    if (generation === cloudCatalogGeneration) cloudCatalogBusy.value = false
  }
}

function resetSyncSetup(): void {
  syncController?.abort()
  syncController = null
  syncMode.value = null
  syncBusy.value = false
  syncError.value = null
}

async function checkSyncMode(activate = false): Promise<void> {
  if (!props.session || session.status !== 'unlocked' || syncBusy.value) return
  const controller = new AbortController()
  syncController = controller
  syncBusy.value = true
  syncError.value = null
  try {
    if (activate) await session.setupInitialCloudSync(controller.signal)
    const mode = await session.cloudSyncMode(controller.signal)
    if (!controller.signal.aborted) syncMode.value = mode
  } catch (cause) {
    if (!controller.signal.aborted) {
      syncError.value = cause instanceof ApiError && cause.code === 'initial_web_sync_requires_empty_account'
        ? setupWords.value.used : setupWords.value.failed
    }
  } finally {
    if (syncController === controller) {
      syncController = null
      syncBusy.value = false
    }
  }
}

function resetCloudCreate(): void {
  cloudCreateController?.abort()
  cloudCreateController = null
  cloudProjectName.value = ''
  cloudCreateBusy.value = false
  cloudCreateSaved.value = false
  cloudCreateError.value = null
}

async function createCloudProject(): Promise<void> {
  const name = cloudProjectName.value.trim()
  if (session.status !== 'unlocked' || (props.session && syncMode.value !== 3) || busy.value || cloudCreateBusy.value || !name) return
  cloudCreateError.value = null
  cloudCreateSaved.value = false
  if (new TextEncoder().encode(name).length > 512) {
    cloudCreateError.value = createWords.value.invalid
    return
  }
  const controller = new AbortController()
  cloudCreateController = controller
  cloudCreateBusy.value = true
  try {
    await session.createCloudProject(name, controller.signal)
    if (controller.signal.aborted || session.status !== 'unlocked') return
    cloudProjectName.value = ''
    cloudCreateSaved.value = true
    cloudCatalogOpen.value = true
    await loadCloudCatalog()
  } catch (cause) {
    if (controller.signal.aborted || session.status !== 'unlocked') return
    if (cause instanceof ApiError && cause.code === 'cloud_project_limit_reached') {
      cloudCreateError.value = createWords.value.limit
    } else if (cause instanceof ApiError && cause.code === 'sync_transport_mode_incompatible') {
      cloudCreateError.value = createWords.value.unavailable
    } else if (cause instanceof BrowserPersistenceError && cause.code === 'writer_busy') {
      cloudCreateError.value = words.value.writerBusy
    } else {
      cloudCreateError.value = createWords.value.failed
    }
  } finally {
    if (cloudCreateController === controller) {
      cloudCreateController = null
      cloudCreateBusy.value = false
      refresh()
    }
  }
}

function toggleCloudCatalog(): void {
  cloudCatalogOpen.value = !cloudCatalogOpen.value
  if (cloudCatalogOpen.value) void loadCloudCatalog()
}

function safeError(value: unknown, action: 'login' | 'unlock'): string {
  if (value instanceof BrowserPersistenceError) {
    if (value.code === 'writer_busy') return words.value.writerBusy
    if (value.code === 'corrupt_record') return t('Хранилище проектов повреждено.')
    if (value.code === 'account_mismatch') return t('Сеанс аккаунта изменился. Войдите снова.')
    return words.value.storageUnavailable
  }
  if (value instanceof KeyNotProvisionedError) return t('Для этого аккаунта нужно настроить сквозное шифрование.')
  if (value instanceof StaleAuthContextError) return t('Сеанс аккаунта изменился. Войдите снова.')
  if (value instanceof ApiError && value.status === 401) return t('Сеанс аккаунта истёк. Войдите снова.')
  if (value instanceof ApiError && value.code === 'legal_document_version_changed') return words.value.stale
  if (value instanceof ApiError) return words.value.connectionFailed
  if (value instanceof CryptoError && value.code === 'decrypt_failed') return words.value.badPassword
  return action === 'unlock' ? t('Произошла непредвиденная ошибка.') : t('Не удалось войти.')
}

async function act(action: 'login' | 'unlock' | 'lock' | 'logout', operation: () => Promise<unknown>): Promise<void> {
  if (busy.value) return
  if ((action === 'lock' || action === 'logout') && workspaceOpen.value && !workspace.value?.canLeave()) return
  if (action === 'lock' || action === 'logout') { resetSyncSetup(); resetCloudCreate(); workspaceOpen.value = false; selectedCloudProject.value = null }
  busy.value = true
  error.value = null
  try { await operation() }
  catch (cause) {
    if (cause instanceof ApiError && cause.code === 'legal_document_version_changed') legalConfirmed.value = {}
    error.value = safeError(cause, action === 'unlock' ? 'unlock' : 'login')
  }
  finally {
    refresh()
    busy.value = false
  }
}

async function signIn(): Promise<void> {
  const input = accountPasswordInput.value
  if (!input || !input.value || !username.value) return
  const password = input.value
  input.value = ''
  clearCloudCatalog()
  legalConfirmed.value = {}
  if (busy.value) return
  busy.value = true
  error.value = null
  needsProvisioning.value = false
  try {
    await session.login(username.value, password, locale.language)
  } catch (cause) {
    if (cause instanceof KeyNotProvisionedError) {
      needsProvisioning.value = true
      return
    }
    if (cause instanceof ApiError && cause.code === 'legal_document_version_changed') legalConfirmed.value = {}
    error.value = safeError(cause, 'login')
  } finally {
    refresh()
    busy.value = false
  }
}
function acceptLegal(): Promise<void> {
  const required = session.legalDocuments.filter(document => document.mandatory)
  if (!required.length || !required.every(document => legalConfirmed.value[document.document_id])) return Promise.resolve()
  return act('login', async () => {
    try {
      await session.acceptLegalDocuments(locale.language, required.map(confirmationFor))
    } catch (cause) {
      if (!(cause instanceof KeyNotProvisionedError)) throw cause
      needsProvisioning.value = true
    }
  })
}
async function provisionEncryption(): Promise<void> {
  const input = provisioningPasswordInput.value
  const confirm = provisioningConfirmInput.value
  if (!input || !input.value || !confirm || !confirm.value) return
  if (input.value !== confirm.value) { provisioningError.value = t('Пароли не совпадают.'); return }
  if (provisioningBusy.value) return
  provisioningBusy.value = true
  provisioningError.value = null
  const password = input.value
  input.value = ''
  confirm.value = ''
  try {
    const result = await session.provision(password)
    recoveryKey.value = result.recoveryKey
    needsProvisioning.value = false
    refresh()
  } catch (cause) {
    provisioningError.value = safeError(cause, 'unlock')
  } finally {
    provisioningBusy.value = false
    refresh()
  }
}

function unlock(): Promise<void> {
  const input = encryptionPasswordInput.value
  if (!input || !input.value) return Promise.resolve()
  const password = input.value
  input.value = ''
  return act('unlock', () => session.unlock(password))
}
function lock(): Promise<void> { return act('lock', () => session.lock()) }
function logout(): Promise<void> { clearCloudCatalog(); return act('logout', () => session.logout()) }
function toggleWorkspace(): void {
  if (workspaceOpen.value && !workspace.value?.canLeave()) return
  workspaceOpen.value = !workspaceOpen.value
}

watch([status, cloudProjects], ([nextStatus, projects]) => {
  if (nextStatus === 'unlocked' && projects?.cloud_project_ids.length && cloudCatalogOpen.value && cloudCatalogController) {
    void loadCloudProjectMetadata(projects.cloud_project_ids, cloudCatalogController, cloudCatalogGeneration)
  }
})

onBeforeUnmount(() => {
  resetSyncSetup()
  resetCloudCreate()
  clearCloudCatalog()
  if (accountPasswordInput.value) accountPasswordInput.value.value = ''
  if (encryptionPasswordInput.value) encryptionPasswordInput.value.value = ''
  unsubscribeRevoked?.()
  if (!props.session) void session.dispose().catch(() => undefined)
})

const unsubscribeRevoked = session.onRevoked?.(refresh)

watch(() => props.workspacePage, workspacePage => {
  if (workspacePage && session.status === 'unlocked' && !cloudCatalogOpen.value) {
    cloudCatalogOpen.value = true
    void loadCloudCatalog()
  }
}, { immediate: true })

watch(status, next => {
  emit('statusChange', next)
  if (next === 'unlocked') void checkSyncMode()
  else { resetSyncSetup(); resetCloudCreate() }
  if (next === 'logged_out' || next === 'legal_required') clearCloudCatalog()
}, { immediate: true })
</script>

<template>
  <section class="settings-card" aria-labelledby="browser-cloud-account-title" data-testid="browser-cloud-account">
    <div class="settings-card__heading">
      <h2 id="browser-cloud-account-title">{{ t('Зашифрованное рабочее пространство') }}</h2>
      <p>{{ t('Веб-приложение') }} · {{ words.cloudProjects }}</p>
    </div>
    <p :data-status="status" aria-live="polite">
      <template v-if="status === 'logged_out'">{{ t('Войдите в аккаунт') }}</template>
      <template v-else-if="status === 'legal_required'">{{ words.legalRequired }}</template>
      <template v-else-if="status === 'authenticated'">{{ t('Подключён аккаунт') }}</template>
      <template v-else-if="status === 'locked'">{{ t('Ключ шифрования заблокирован. Введите пароль шифрования.') }}</template>
      <template v-else>{{ words.unlocked }}</template>
    </p>
    <p v-if="connectedUsername">{{ t('Подключён аккаунт') }}: <strong>{{ connectedUsername }}</strong></p>
    <p v-if="error" role="alert">{{ error }}</p>
    <form v-if="status === 'logged_out'" class="browser-cloud-account__inputs" @submit.prevent="signIn">
      <label>{{ t('Имя пользователя или e-mail') }} <input v-model="username" autocomplete="username" /></label>
      <label>{{ t('Пароль аккаунта') }} <input ref="accountPasswordInput" type="password" autocomplete="current-password" required /></label>
      <button class="nf-button" type="submit" :disabled="busy || !username">{{ t('Войти в облачный аккаунт') }}</button>
      <RouterLink v-if="props.session" :to="{ name: 'web-password-reset' }">{{ t('Восстановление пароля') }}</RouterLink>
    </form>
    <div v-else-if="status === 'legal_required'" class="browser-cloud-account__legal">
      <article v-for="document in session.legalDocuments" :key="`${document.document_id}:${document.revision}`">
        <h3>{{ document.title }}</h3>
        <p class="browser-cloud-account__legal-text">{{ document.content }}</p>
        <label v-if="document.mandatory">
          <input v-model="legalConfirmed[document.document_id]" type="checkbox" />
          {{ words.acceptRevision }}
        </label>
      </article>
      <button class="nf-button" type="button" :disabled="busy || session.legalDocuments.filter(document => document.mandatory).some(document => !legalConfirmed[document.document_id])" @click="acceptLegal">
        {{ words.acceptCurrent }}
      </button>
      <button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="logout">{{ t('Выйти из аккаунта') }}</button>
    </div>
    <div v-else class="browser-cloud-account__inputs">
      <p class="browser-cloud-account__online-policy">{{ words.onlineOnly }}</p>
      <button class="nf-button nf-button--secondary" type="button" :disabled="busy || cloudCatalogBusy" data-testid="cloud-catalog-toggle" @click="toggleCloudCatalog">
        {{ cloudCatalogOpen ? t('Скрыть') : words.cloudProjects }}
      </button>
      <div v-if="props.session && status === 'unlocked' && syncMode !== 3" class="browser-cloud-account__create">
        <p v-if="syncBusy" role="status">{{ setupWords.busy }}</p>
        <p v-else-if="syncError" role="alert">{{ syncError }}</p>
        <p v-else-if="syncMode !== null">{{ setupWords.hint }}</p>
        <button v-if="!syncBusy" class="nf-button" type="button" :disabled="busy" data-testid="cloud-sync-setup"
          @click="checkSyncMode(syncMode !== null)">{{ syncMode === null ? t('Повторить') : setupWords.action }}</button>
      </div>
      <form v-if="status === 'unlocked' && (!props.session || syncMode === 3)" class="browser-cloud-account__inputs browser-cloud-account__create"
        data-testid="cloud-project-create" :aria-label="createWords.title" @submit.prevent="createCloudProject">
        <label>{{ createWords.name }} <input v-model="cloudProjectName" required :disabled="cloudCreateBusy || busy" /></label>
        <button class="nf-button" type="submit" :disabled="cloudCreateBusy || busy || !cloudProjectName.trim()">{{ createWords.title }}</button>
        <p v-if="cloudCreateBusy" role="status">{{ createWords.saving }}</p>
        <p v-else-if="cloudCreateSaved" role="status">{{ createWords.saved }}</p>
        <p v-if="cloudCreateError" role="alert">{{ cloudCreateError }}</p>
      </form>
      <section v-if="cloudCatalogOpen" class="browser-cloud-account__catalog" :aria-label="words.cloudProjects" data-testid="server-cloud-project-catalog">
        <div class="browser-cloud-account__catalog-heading">
          <h3>{{ words.cloudProjects }}</h3>
          <button class="nf-button nf-button--secondary" type="button" :disabled="cloudCatalogBusy" @click="loadCloudCatalog">{{ words.refreshProjects }}</button>
        </div>
        <p v-if="cloudCatalogBusy" role="status">{{ t('Загружаем с сервера…') }}</p>
        <p v-if="cloudCatalogError" role="alert">{{ cloudCatalogError }}</p>
        <template v-if="cloudProjects">
          <p>{{ words.projectCount.replace('{count}', String(cloudProjects.cloud_project_count)).replace('{limit}', String(cloudProjects.max_cloud_projects)) }}</p>
          <p v-if="cloudProjects.cloud_project_ids.length === 0">{{ words.noCloudProjects }}</p>
          <ul v-else>
            <li v-for="id in cloudProjects.cloud_project_ids" :key="id">
              <code>{{ id }}</code>
              <p v-if="status === 'unlocked' && cloudProjectMetadataLoading[id]" role="status">{{ t('Загружаем с сервера…') }}</p>
              <p v-else-if="status === 'unlocked' && cloudProjectMetadataError[id]" role="alert">{{ cloudProjectMetadataError[id] }}</p>
              <p v-else-if="status === 'unlocked' && cloudProjectMetadata[id]">
                {{ cloudProjectMetadata[id]!.metadata.name }}
                <span v-if="cloudProjectMetadata[id]!.metadata.cover_reference"> · {{ t('Обложка') }}: {{ cloudProjectMetadata[id]!.metadata.cover_reference!.blob_id }}</span>
                <span v-else> · —</span>
              </p>
              <button class="nf-button nf-button--secondary" type="button" :disabled="status !== 'unlocked'" :data-cloud-project-id="id" @click="selectedCloudProject = id">{{ words.openCloudProject }}</button>
            </li>
          </ul>
          <p>{{ words.projectIdsOnly }}</p>
          <BrowserCloudProjectRead v-if="selectedCloudProject && status === 'unlocked'" :key="selectedCloudProject" :project-id="selectedCloudProject" :session="session" :auto-open-recovery="true" @invalidated="refresh(); clearCloudCatalog()" />
        </template>
      </section>
      <form v-if="needsProvisioning" class="browser-cloud-account__inputs" @submit.prevent="provisionEncryption">
        <p>{{ t('Создать ключ шифрования') }}</p>
        <label>{{ t('Пароль шифрования') }} <input ref="provisioningPasswordInput" type="password" autocomplete="new-password" required /></label>
        <label>{{ t('Повторите пароль') }} <input ref="provisioningConfirmInput" type="password" autocomplete="new-password" required /></label>
        <button class="nf-button" type="submit" :disabled="provisioningBusy">{{ t('Создать ключ шифрования') }}</button>
        <p v-if="provisioningError" role="alert">{{ provisioningError }}</p>
      </form>
      <form v-else-if="status !== 'unlocked'" class="browser-cloud-account__inputs" @submit.prevent="unlock">
        <label>{{ t('Пароль шифрования') }} <input ref="encryptionPasswordInput" type="password" autocomplete="off" required /></label>
        <button class="nf-button" type="submit" :disabled="busy">{{ t('Разблокировать') }}</button>
      </form>
      <section v-if="recoveryKey" class="browser-cloud-account__catalog" aria-live="polite">
        <h3>{{ t('Ключ восстановления') }}</h3>
        <textarea readonly style="width:100%" :value="recoveryKey"></textarea>
        <button class="nf-button" type="button" @click="recoveryKey = null">{{ t('Закрыть') }}</button>
      </section>
      <button v-if="status === 'unlocked'" class="nf-button nf-button--secondary" type="button" data-testid="cloud-account-lock" :disabled="busy" @click="lock">{{ t('Заблокировать ключ') }}</button>
      <button v-if="status === 'unlocked' && !props.session" class="nf-button" type="button" :disabled="busy" @click="toggleWorkspace">
        {{ workspaceOpen ? t('Закрыть рабочее пространство') : words.localWorkspace }}
      </button>
      <button class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="logout">{{ t('Выйти из аккаунта') }}</button>
    </div>
    <BrowserEncryptedWorkspace v-if="status === 'unlocked' && workspaceOpen" ref="workspace" :session="session" @close="workspaceOpen = false" />
  </section>
</template>

<style scoped>
.browser-cloud-account__inputs { display: flex; flex-wrap: wrap; gap: .75rem; align-items: end; }
.browser-cloud-account__inputs label { display: grid; gap: .25rem; min-width: 14rem; }
.browser-cloud-account__legal { display: grid; gap: 1rem; }
.browser-cloud-account__legal article { padding: 1rem; border: 1px solid var(--nf-color-border); border-radius: .5rem; }
.browser-cloud-account__legal-text { white-space: pre-line; }
.browser-cloud-account__create { flex-basis: 100%; }
.browser-cloud-account__online-policy { flex-basis: 100%; }
.browser-cloud-account__catalog { flex-basis: 100%; display: grid; gap: .5rem; padding: 1rem; border: 1px solid var(--nf-color-border); border-radius: .5rem; }
.browser-cloud-account__catalog-heading { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
.browser-cloud-account__catalog h3, .browser-cloud-account__catalog p { margin: 0; }
[role="alert"] { color: var(--ion-color-danger); }
</style>
