<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ApiError } from '@/api/client'
import { invitationApi } from '@/api/invitations'
import { confirmationFor, type LegalDocument } from '@/api/legalDocuments'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'

type Copy = { title: string; detail: string; loading: string; password: string; confirm: string; accept: string; submit: string; invalid: string; mismatch: string; required: string; stale: string; failed: string; done: string; revision: string; documentLanguage: string; retry: string }
const copy: Record<SupportedLanguage, Copy> = {
  ru: { title: 'Приглашение в worta', detail: 'До активации ознакомьтесь с условиями закрытого тестирования и подтвердите каждую обязательную редакцию.', loading: 'Загружаем действующие условия…', password: 'Новый пароль (не менее 15 символов)', confirm: 'Повторите пароль', accept: 'Я ознакомился(-ась) и принимаю эту редакцию', submit: 'Принять условия и активировать аккаунт', invalid: 'Ссылка недействительна или срок её действия истёк.', mismatch: 'Пароли не совпадают.', required: 'Подтвердите каждый обязательный документ.', stale: 'Редакция условий изменилась. Ознакомьтесь с обновлённым текстом и подтвердите его заново.', failed: 'Не удалось принять приглашение. Данные не изменены; повторите попытку.', done: 'Условия приняты, пароль создан. Аккаунт готов к входу в тестовый Web-клиент.', revision: 'Редакция', documentLanguage: 'Язык документа', retry: 'Повторить загрузку' },
  en: { title: 'worta invitation', detail: 'Before activation, review the private-beta terms and confirm every required version.', loading: 'Loading current terms…', password: 'New password (at least 15 characters)', confirm: 'Repeat password', accept: 'I have read and accept this version', submit: 'Accept terms and activate account', invalid: 'This link is invalid or has expired.', mismatch: 'Passwords do not match.', required: 'Confirm every required document.', stale: 'The terms changed. Review the updated text and confirm it again.', failed: 'Could not accept the invitation. Nothing was changed; try again.', done: 'Terms accepted and password set. The account can now sign in to the test Web client.', revision: 'Revision', documentLanguage: 'Document language', retry: 'Try loading again' },
  es: { title: 'Invitación a worta', detail: 'Antes de activar la cuenta, revisa las condiciones de la prueba privada y confirma cada versión obligatoria.', loading: 'Cargando las condiciones vigentes…', password: 'Nueva contraseña (al menos 15 caracteres)', confirm: 'Repite la contraseña', accept: 'He leído y acepto esta versión', submit: 'Aceptar y activar la cuenta', invalid: 'Este enlace no es válido o ha caducado.', mismatch: 'Las contraseñas no coinciden.', required: 'Confirma todos los documentos obligatorios.', stale: 'Las condiciones han cambiado. Revisa el texto actualizado y confírmalo de nuevo.', failed: 'No se pudo aceptar la invitación. No se modificó nada; inténtalo de nuevo.', done: 'Condiciones aceptadas y contraseña establecida. Ya puedes entrar en el cliente Web de prueba.', revision: 'Revisión', documentLanguage: 'Idioma del documento', retry: 'Volver a cargar' },
  de: { title: 'worta-Einladung', detail: 'Lies vor der Aktivierung die Bedingungen des geschlossenen Tests und bestätige jede erforderliche Fassung.', loading: 'Aktuelle Bedingungen werden geladen…', password: 'Neues Passwort (mindestens 15 Zeichen)', confirm: 'Passwort wiederholen', accept: 'Ich habe diese Fassung gelesen und akzeptiere sie', submit: 'Bedingungen akzeptieren und Konto aktivieren', invalid: 'Dieser Link ist ungültig oder abgelaufen.', mismatch: 'Die Passwörter stimmen nicht überein.', required: 'Bestätige jedes erforderliche Dokument.', stale: 'Die Bedingungen wurden geändert. Lies den aktualisierten Text und bestätige ihn erneut.', failed: 'Die Einladung konnte nicht angenommen werden. Es wurde nichts geändert; versuche es erneut.', done: 'Bedingungen akzeptiert und Passwort festgelegt. Das Konto kann sich jetzt im Test-Webclient anmelden.', revision: 'Fassung', documentLanguage: 'Dokumentsprache', retry: 'Erneut laden' },
  fr: { title: 'Invitation worta', detail: 'Avant l’activation, consultez les conditions du test privé et confirmez chaque version obligatoire.', loading: 'Chargement des conditions en vigueur…', password: 'Nouveau mot de passe (15 caractères minimum)', confirm: 'Répétez le mot de passe', accept: 'J’ai lu et j’accepte cette version', submit: 'Accepter et activer le compte', invalid: 'Ce lien est invalide ou a expiré.', mismatch: 'Les mots de passe ne correspondent pas.', required: 'Confirmez chaque document obligatoire.', stale: 'Les conditions ont changé. Consultez le texte mis à jour et confirmez-le à nouveau.', failed: 'Impossible d’accepter l’invitation. Aucune donnée n’a été modifiée ; réessayez.', done: 'Conditions acceptées et mot de passe défini. Le compte peut se connecter au client Web de test.', revision: 'Version', documentLanguage: 'Langue du document', retry: 'Recharger' },
  pt_BR: { title: 'Convite para worta', detail: 'Antes da ativação, leia os termos do teste fechado e confirme cada versão obrigatória.', loading: 'Carregando os termos atuais…', password: 'Nova senha (pelo menos 15 caracteres)', confirm: 'Repita a senha', accept: 'Li e aceito esta versão', submit: 'Aceitar e ativar a conta', invalid: 'Este link é inválido ou expirou.', mismatch: 'As senhas não coincidem.', required: 'Confirme todos os documentos obrigatórios.', stale: 'Os termos mudaram. Leia o texto atualizado e confirme novamente.', failed: 'Não foi possível aceitar o convite. Nada foi alterado; tente novamente.', done: 'Termos aceitos e senha definida. A conta já pode entrar no cliente Web de teste.', revision: 'Revisão', documentLanguage: 'Idioma do documento', retry: 'Carregar novamente' },
}

const locale = useLocaleStore()
const words = computed(() => copy[locale.language])
const passwordInput = ref<HTMLInputElement | null>(null)
const confirmInput = ref<HTMLInputElement | null>(null)
const busy = ref(false)
const loading = ref(false)
const complete = ref(false)
const hasInvitation = ref(false)
const loadFailed = ref(false)
const documents = ref<LegalDocument[]>([])
const confirmed = ref<Record<string, boolean>>({})
const error = ref('')
let invitationToken: string | null = null

const mandatory = computed(() => documents.value.filter(document => document.mandatory))
const allConfirmed = computed(() => mandatory.value.length > 0
  && mandatory.value.every(document => confirmed.value[document.document_id] === true))

async function loadDocuments(stale = false): Promise<void> {
  if (!invitationToken) return
  loading.value = true
  loadFailed.value = false
  error.value = stale ? words.value.stale : ''
  confirmed.value = {}
  try {
    const status = await invitationApi.documents(invitationToken, locale.language)
    documents.value = status.documents
  } catch (cause) {
    documents.value = []
    if (cause instanceof ApiError && cause.status === 0) {
      loadFailed.value = true
      error.value = words.value.failed
    } else {
      invitationToken = null
      hasInvitation.value = false
      error.value = words.value.invalid
    }
  } finally { loading.value = false }
}

onMounted(() => {
  invitationToken = new URLSearchParams(window.location.hash.slice(1)).get('token')
  window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search)
  hasInvitation.value = Boolean(invitationToken)
  if (!invitationToken) error.value = words.value.invalid
  else void loadDocuments()
})
onBeforeUnmount(() => {
  invitationToken = null
  if (passwordInput.value) passwordInput.value.value = ''
  if (confirmInput.value) confirmInput.value.value = ''
})

async function submit(): Promise<void> {
  if (busy.value || !invitationToken || !passwordInput.value || !confirmInput.value) return
  if (!allConfirmed.value) { error.value = words.value.required; return }
  const password = passwordInput.value.value
  const confirmation = confirmInput.value.value
  passwordInput.value.value = ''
  confirmInput.value.value = ''
  if (password !== confirmation) { error.value = words.value.mismatch; return }
  busy.value = true
  error.value = ''
  try {
    await invitationApi.accept(
      invitationToken, password, locale.language, mandatory.value.map(confirmationFor),
    )
    invitationToken = null
    hasInvitation.value = false
    complete.value = true
  } catch (cause) {
    if (cause instanceof ApiError && cause.code === 'legal_document_version_changed') await loadDocuments(true)
    else error.value = words.value.failed
  } finally { busy.value = false }
}
</script>

<template>
  <main class="ion-page invitation-page">
    <section>
      <h1>{{ words.title }}</h1>
      <p>{{ words.detail }}</p>
      <p v-if="loading" role="status">{{ words.loading }}</p>
      <p v-if="complete" role="status">{{ words.done }}</p>
      <p v-if="error" role="alert">{{ error }}</p>
      <button v-if="hasInvitation && loadFailed" class="nf-button" type="button" @click="loadDocuments()">{{ words.retry }}</button>
      <form v-if="hasInvitation && !complete && !loading && documents.length" @submit.prevent="submit">
        <article v-for="document in documents" :key="`${document.document_id}:${document.revision}:${document.language}`" class="legal-document">
          <h2>{{ document.title }}</h2>
          <p class="legal-document__meta">{{ words.revision }}: {{ document.revision }} · {{ words.documentLanguage }}: {{ document.language }}</p>
          <div class="legal-document__content">{{ document.content }}</div>
          <label v-if="document.mandatory" class="legal-document__confirmation">
            <input v-model="confirmed[document.document_id]" type="checkbox" />
            {{ words.accept }}
          </label>
        </article>
        <label>{{ words.password }}<input ref="passwordInput" type="password" autocomplete="new-password" minlength="15" required /></label>
        <label>{{ words.confirm }}<input ref="confirmInput" type="password" autocomplete="new-password" minlength="15" required /></label>
        <button class="nf-button" type="submit" :disabled="busy || !allConfirmed">{{ words.submit }}</button>
      </form>
    </section>
  </main>
</template>

<style scoped>
.invitation-page { min-height: 100dvh; display: grid; place-items: center; padding: 2rem; }
.invitation-page > section { width: min(44rem, 100%); }
.invitation-page form, .invitation-page label { display: grid; gap: .7rem; }
.invitation-page form { margin-top: 1.5rem; }
.invitation-page input[type="password"] { padding: .65rem; border: 1px solid var(--nf-color-border); border-radius: .4rem; }
.legal-document { margin: 1rem 0; padding: 1rem; border: 1px solid var(--nf-color-border); border-radius: .6rem; }
.legal-document h2 { margin-top: 0; }
.legal-document__meta { color: var(--nf-color-text-muted); font-size: .875rem; }
.legal-document__content { white-space: pre-line; line-height: 1.55; }
.legal-document__confirmation { grid-template-columns: auto 1fr; align-items: start; margin-top: 1rem; }
</style>
