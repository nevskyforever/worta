<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef } from 'vue'
import { RouterLink } from 'vue-router'
import { ApiError } from '@/api/client'
import { passwordResetApi } from '@/api/passwordReset'
import { webAccountSession } from '@/platform/web/webAccountSession'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'

const copy = {
  ru: { send: 'Запросить ссылку', sent: 'Если восстановление по этому адресу доступно, вы получите письмо.', e2ee: 'Для разблокировки проектов по-прежнему нужен пароль шифрования или ключ восстановления.', invalid: 'Ссылка недействительна или истекла. Запросите новую.', failed: 'Запрос не выполнен. Попробуйте ещё раз.', done: 'Пароль аккаунта изменён. Войдите с новым паролем.' },
  en: { send: 'Request a reset link', sent: 'If recovery is available for this address, you will receive an email.', e2ee: 'You still need your encryption password or recovery key to unlock projects.', invalid: 'The link is invalid or expired. Request a new one.', failed: 'The request failed. Try again.', done: 'Your account password has changed. Sign in with the new password.' },
  es: { send: 'Solicitar enlace', sent: 'Si esta dirección admite la recuperación, recibirás un correo.', e2ee: 'Para desbloquear los proyectos necesitas la contraseña de cifrado o la clave de recuperación.', invalid: 'El enlace no es válido o ha caducado. Solicita otro.', failed: 'La solicitud ha fallado. Inténtalo de nuevo.', done: 'Se ha cambiado la contraseña. Inicia sesión con la nueva.' },
  de: { send: 'Link anfordern', sent: 'Wenn die Wiederherstellung für diese Adresse verfügbar ist, erhältst du eine E-Mail.', e2ee: 'Zum Entsperren von Projekten brauchst du weiterhin das Verschlüsselungspasswort oder den Wiederherstellungsschlüssel.', invalid: 'Der Link ist ungültig oder abgelaufen. Fordere einen neuen an.', failed: 'Die Anfrage ist fehlgeschlagen. Versuche es erneut.', done: 'Das Kontopasswort wurde geändert. Melde dich mit dem neuen Passwort an.' },
  fr: { send: 'Demander un lien', sent: 'Si cette adresse permet la récupération, vous recevrez un e-mail.', e2ee: 'Le mot de passe de chiffrement ou la clé de récupération reste nécessaire pour déverrouiller les projets.', invalid: 'Le lien est invalide ou expiré. Demandez-en un nouveau.', failed: 'La demande a échoué. Réessayez.', done: 'Le mot de passe du compte a été modifié. Connectez-vous avec le nouveau mot de passe.' },
  pt_BR: { send: 'Solicitar link', sent: 'Se a recuperação estiver disponível para este endereço, você receberá um e-mail.', e2ee: 'A senha de criptografia ou a chave de recuperação continua necessária para desbloquear os projetos.', invalid: 'O link é inválido ou expirou. Solicite outro.', failed: 'A solicitação falhou. Tente novamente.', done: 'A senha da conta foi alterada. Entre com a nova senha.' },
} satisfies Record<SupportedLanguage, Record<'send' | 'sent' | 'e2ee' | 'invalid' | 'failed' | 'done', string>>

const locale = useLocaleStore()
const t = locale.translate
const words = computed(() => copy[locale.language])
const fragment = new URLSearchParams(window.location.hash.slice(1))
const query = new URLSearchParams(window.location.search)
const token = shallowRef(fragment.get('token') ?? query.get('token'))
if (fragment.has('token') || query.has('token')) {
  window.history.replaceState(window.history.state, '', window.location.pathname)
}

const email = ref('')
const password = ref<HTMLInputElement | null>(null)
const confirmation = ref<HTMLInputElement | null>(null)
const busy = ref(false)
const error = ref('')
const result = ref<'sent' | 'done' | null>(null)
const controller = new AbortController()
onBeforeUnmount(() => {
  controller.abort()
  token.value = null
  if (password.value) password.value.value = ''
  if (confirmation.value) confirmation.value.value = ''
})

async function submit(): Promise<void> {
  if (busy.value || result.value) return
  error.value = ''
  busy.value = true
  const resetToken = token.value
  try {
    if (resetToken) {
      const value = password.value?.value ?? ''
      if (value !== confirmation.value?.value) {
        error.value = t('Пароли не совпадают.')
        return
      }
      if (password.value) password.value.value = ''
      if (confirmation.value) confirmation.value.value = ''
      const response = await passwordResetApi.confirm(resetToken, value, controller.signal)
      if (response.code !== 'password_reset_complete') throw new TypeError('invalid_password_reset_response')
      token.value = null
      result.value = 'done'
      await webAccountSession().logout().catch(() => undefined)
    } else {
      const response = await passwordResetApi.request(email.value.trim(), controller.signal)
      if (response.code !== 'password_reset_request_accepted') throw new TypeError('invalid_password_reset_response')
      email.value = ''
      result.value = 'sent'
    }
  } catch (cause) {
    if (controller.signal.aborted) return
    if (cause instanceof ApiError && cause.code === 'invalid_or_expired_token') {
      token.value = null
      error.value = words.value.invalid
    } else error.value = words.value.failed
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <main class="ion-page password-reset">
    <section>
      <h1>{{ t('Восстановление пароля') }} · worta</h1>
      <p>{{ words.e2ee }}</p>
      <p v-if="error" role="alert">{{ error }}</p>
      <p v-if="result" role="status">{{ result === 'sent' ? words.sent : words.done }}</p>
      <form v-else :aria-busy="busy" @submit.prevent="submit">
        <template v-if="token">
          <label>{{ t('Новый пароль (не менее 15 символов)') }}
            <input ref="password" type="password" autocomplete="new-password" minlength="15" maxlength="1024" required />
          </label>
          <label>{{ t('Повторите пароль') }}
            <input ref="confirmation" type="password" autocomplete="new-password" minlength="15" maxlength="1024" required />
          </label>
        </template>
        <label v-else>{{ t('Электронная почта') }}
          <input v-model="email" type="email" autocomplete="email" maxlength="320" required />
        </label>
        <button class="nf-button" type="submit" :disabled="busy">{{ token ? t('Сохранить') : words.send }}</button>
      </form>
      <RouterLink :to="{ name: 'web-login' }">{{ t('Войти в облачный аккаунт') }}</RouterLink>
    </section>
  </main>
</template>

<style scoped>
.password-reset { display: block; height: 100%; overflow: auto; padding: clamp(1rem, 4vw, 3rem); }
.password-reset section { max-width: 30rem; margin: 0 auto; display: grid; gap: 1rem; }
.password-reset form, .password-reset label { display: grid; gap: .75rem; }
</style>
