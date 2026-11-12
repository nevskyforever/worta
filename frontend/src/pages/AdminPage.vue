<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { adminApi, adminSession, type AdminInvitation, type AdminUser, type RegistrationSettings, type ReservedUsername } from '@/api/admin'
import { useLocaleStore } from '@/stores/locale'
import type { SupportedLanguage } from '@/types/api'

const invitationCopy: Record<SupportedLanguage, { title: string; description: string; create: string; sent: string; revoke: string; confirm: string; failed: string; pending: string; accepted: string; revoked: string; expired: string }> = {
  ru: { title: 'Приглашения в закрытое тестирование', description: 'Аккаунт создаётся в ожидании. Ссылка отправляется на указанный email и действует 72 часа.', create: 'Создать аккаунт и отправить приглашение', sent: 'Приглашение создано и передано на отправку.', revoke: 'Отозвать', confirm: 'Отозвать это приглашение?', failed: 'Не удалось выполнить действие с приглашением.', pending: 'Ожидает', accepted: 'Принято', revoked: 'Отозвано', expired: 'Истекло' },
  en: { title: 'Private testing invitations', description: 'A pending account is created. The link is emailed and expires after 72 hours.', create: 'Create account and send invitation', sent: 'Invitation created and queued for delivery.', revoke: 'Revoke', confirm: 'Revoke this invitation?', failed: 'Invitation action failed.', pending: 'Pending', accepted: 'Accepted', revoked: 'Revoked', expired: 'Expired' },
  es: { title: 'Invitaciones a pruebas privadas', description: 'Se crea una cuenta pendiente. El enlace se envía por correo y caduca en 72 horas.', create: 'Crear cuenta y enviar invitación', sent: 'Invitación creada y puesta en cola.', revoke: 'Revocar', confirm: '¿Revocar esta invitación?', failed: 'No se pudo gestionar la invitación.', pending: 'Pendiente', accepted: 'Aceptada', revoked: 'Revocada', expired: 'Caducada' },
  de: { title: 'Einladungen zum geschlossenen Test', description: 'Ein ausstehendes Konto wird angelegt. Der Link wird per E-Mail gesendet und läuft nach 72 Stunden ab.', create: 'Konto anlegen und Einladung senden', sent: 'Einladung erstellt und zum Versand vorgemerkt.', revoke: 'Widerrufen', confirm: 'Diese Einladung widerrufen?', failed: 'Einladungsaktion fehlgeschlagen.', pending: 'Ausstehend', accepted: 'Angenommen', revoked: 'Widerrufen', expired: 'Abgelaufen' },
  fr: { title: 'Invitations au test privé', description: 'Un compte en attente est créé. Le lien est envoyé par email et expire après 72 heures.', create: 'Créer le compte et envoyer l’invitation', sent: 'Invitation créée et mise en attente d’envoi.', revoke: 'Révoquer', confirm: 'Révoquer cette invitation ?', failed: 'Action sur l’invitation impossible.', pending: 'En attente', accepted: 'Acceptée', revoked: 'Révoquée', expired: 'Expirée' },
  pt_BR: { title: 'Convites para teste fechado', description: 'Uma conta pendente é criada. O link é enviado por email e expira em 72 horas.', create: 'Criar conta e enviar convite', sent: 'Convite criado e colocado na fila de envio.', revoke: 'Revogar', confirm: 'Revogar este convite?', failed: 'Não foi possível gerenciar o convite.', pending: 'Pendente', accepted: 'Aceito', revoked: 'Revogado', expired: 'Expirado' },
}

const router = useRouter(); const locale = useLocaleStore(); const t = locale.translate; const users = ref<AdminUser[]>([]); const total = ref(0); const search = ref(''); const status = ref(''); const role = ref(''); const offset = ref(0); const pageSize = 50; const registration = ref<RegistrationSettings | null>(null); const globalLimit = ref(20); const reserved = ref<ReservedUsername[]>([]); const reservedName = ref(''); const error = ref('')
const words = computed(() => invitationCopy[locale.language])
const invitations = ref<AdminInvitation[]>([])
const inviteUsername = ref('')
const inviteEmail = ref('')
const inviteBusy = ref(false)
const inviteNotice = ref('')
async function loadUsers(){ const q=new URLSearchParams({limit:String(pageSize),offset:String(offset.value)});if(search.value)q.set('search',search.value);if(status.value)q.set('status',status.value);if(role.value)q.set('role',role.value);const result=await adminApi.users(q.toString());if(result.total > 0 && offset.value >= result.total){offset.value=Math.max(0,Math.floor((result.total-1)/pageSize)*pageSize);return loadUsers()}users.value=result.users;total.value=result.total }
async function load(){try{await Promise.all([loadUsers(),adminApi.invitations().then(v=>invitations.value=v),adminApi.registration().then(v=>registration.value=v),adminApi.limits().then(v=>globalLimit.value=v.max_cloud_projects),adminApi.reserved().then(v=>reserved.value=v)])}catch(e){error.value=e instanceof Error?e.message:'Ошибка загрузки'}}
async function createInvite(){if(inviteBusy.value)return;inviteBusy.value=true;inviteNotice.value='';try{await adminApi.issueInvitation(inviteUsername.value,inviteEmail.value);inviteUsername.value='';inviteEmail.value='';inviteNotice.value=words.value.sent;await Promise.all([loadUsers(),adminApi.invitations().then(v=>invitations.value=v)])}catch{error.value=words.value.failed}finally{inviteBusy.value=false}}
async function revokeInvite(id:string){if(!confirm(words.value.confirm))return;try{await adminApi.revokeInvitation(id);invitations.value=await adminApi.invitations()}catch{error.value=words.value.failed}}
async function action(user:AdminUser, action:'approve'|'reject'|'block'|'unblock'|'revoke'){if(!confirm(`Подтвердить действие: ${action}?`))return; if(action==='revoke')await adminApi.revoke(user.id);else await adminApi.lifecycle(user.id,action);await loadUsers()}
async function saveRegistration(){if(registration.value)registration.value=await adminApi.saveRegistration({mode:'closed',max_users:registration.value.max_users})}
async function saveLimit(){await adminApi.saveLimits(globalLimit.value)}
async function setOverride(user:AdminUser){const raw=prompt('Лимит проектов (пусто — наследовать):',user.max_cloud_projects_override?.toString()??'');if(raw===null)return;await adminApi.userLimit(user.id,raw.trim()===''?null:Number(raw));await loadUsers()}
async function addReserved(){if(!reservedName.value.trim())return;await adminApi.addReserved(reservedName.value);reservedName.value='';reserved.value=await adminApi.reserved()}
async function removeReserved(name:string){if(confirm(`Удалить резервирование ${name}?`)){await adminApi.removeReserved(name);reserved.value=await adminApi.reserved()}}
async function logout(){try{await adminSession.logout()}finally{router.replace('/admin/login')}}
function applyFilters(){offset.value=0;void loadUsers()}
function previous(){offset.value=Math.max(0,offset.value-pageSize);void loadUsers()}
function next(){if(offset.value+pageSize<total.value){offset.value+=pageSize;void loadUsers()}}
onMounted(load)
</script>
<template><main class="admin"><header><h1>{{t('Администрирование')}}</h1><button class="nf-button" @click="logout">{{t('Выйти')}}</button></header><p v-if="error" role="alert">{{error}}</p><section><h2>{{t('Пользователи')}} ({{total}})</h2><form @submit.prevent="applyFilters"><input v-model="search" :aria-label="t('Поиск')" :placeholder="t('Поиск')"/><select v-model="status" :aria-label="t('Статус')"><option value="">{{t('Все статусы')}}</option><option>pending</option><option>active</option><option>rejected</option><option>blocked</option></select><select v-model="role" :aria-label="t('Роль')"><option value="">{{t('Все роли')}}</option><option>user</option><option>admin</option></select><button>{{t('Найти')}}</button></form><div class="table-scroll" tabindex="0"><table><thead><tr><th>{{t('Пользователь')}}</th><th>{{t('Статус')}}</th><th>{{t('Лимит')}}</th><th>{{t('Действия')}}</th></tr></thead><tbody><tr v-for="user in users" :key="user.id"><td>{{user.username}}<br/><small>{{user.email}} · {{user.role}} · {{user.email_verified?t('email подтверждён'):t('email не подтверждён')}}</small></td><td>{{user.status}}</td><td>{{user.effective_max_cloud_projects}} <small>{{user.max_cloud_projects_override===null?t('(общий)'):t('(override)')}}</small></td><td v-if="user.role==='admin'">{{t('Защищён')}}</td><td v-else><button v-if="user.status==='pending'||user.status==='rejected'" @click="action(user,'approve')">{{t('Одобрить')}}</button><button v-if="user.status==='pending'" @click="action(user,'reject')">{{t('Отклонить')}}</button><button v-if="user.status==='active'" @click="action(user,'block')">{{t('Блокировать')}}</button><button @click="action(user,'revoke')">{{t('Сессии')}}</button><button @click="setOverride(user)">{{t('Лимит')}}</button></td></tr></tbody></table></div><nav :aria-label="t('Пагинация')"><button :disabled="offset===0" @click="previous">{{t('Назад')}}</button><span>{{total===0?0:offset+1}}–{{Math.min(offset+pageSize,total)}} / {{total}}</span><button :disabled="offset+pageSize>=total" @click="next">{{t('Далее')}}</button></nav></section><section class="invite-section"><h2>{{words.title}}</h2><p>{{words.description}}</p><form class="invite-form" @submit.prevent="createInvite"><label>{{t('Имя пользователя')}}<input v-model="inviteUsername" autocomplete="off" required /></label><label>Email<input v-model="inviteEmail" type="email" autocomplete="off" required /></label><button class="nf-button" :disabled="inviteBusy">{{words.create}}</button></form><p v-if="inviteNotice" role="status">{{inviteNotice}}</p><div class="table-scroll" tabindex="0"><table><thead><tr><th>{{t('Пользователь')}}</th><th>{{t('Статус')}}</th><th>{{t('Действия')}}</th></tr></thead><tbody><tr v-for="invitation in invitations" :key="invitation.id"><td>{{invitation.username}}<br/><small>{{invitation.email}} · {{new Date(invitation.expires_at).toLocaleString(locale.localeTag)}}</small></td><td>{{words[invitation.status]}}</td><td><button v-if="invitation.status==='pending'" @click="revokeInvite(invitation.id)">{{words.revoke}}</button></td></tr></tbody></table></div></section><section v-if="registration"><h2>{{t('Регистрация')}}</h2><strong>{{t('Закрытая')}}</strong><label>{{t('Максимум пользователей')}} <input v-model.number="registration.max_users" type="number" min="0" /></label><small>{{t('Активных')}}: {{registration.active_users}}. {{t('Уменьшение не отключает существующие аккаунты.')}}</small><button @click="saveRegistration">{{t('Сохранить')}}</button></section><section><h2>{{t('Лимиты')}}</h2><label>{{t('Максимум cloud-проектов')}} <input v-model.number="globalLimit" type="number" min="0"/></label><button @click="saveLimit">{{t('Сохранить')}}</button></section><section><h2>{{t('Зарезервированные имена')}}</h2><form @submit.prevent="addReserved"><input v-model="reservedName" :aria-label="t('Имя')"/><button>{{t('Добавить')}}</button></form><ul><li v-for="item in reserved" :key="item.username_normalized">{{item.username_normalized}} <button @click="removeReserved(item.username_normalized)">{{t('Удалить')}}</button></li></ul></section></main></template>
<style scoped>
.admin { box-sizing: border-box; width: min(100%, 80rem); height: 100dvh; overflow-y: auto; margin: 0 auto; padding: clamp(1rem, 3vw, 2rem); display: grid; gap: 1.25rem; align-content: start; color: var(--nf-color-text); font-size: 1rem; line-height: 1.5; }
.admin header { display: flex; flex-wrap: wrap; gap: 1rem; justify-content: space-between; align-items: center; }
h1, h2, p { margin: 0; }
h1 { font-size: clamp(1.5rem, 3vw, 2rem); }
h2 { font-size: 1.2rem; }
section { min-width: 0; display: grid; gap: .85rem; padding: clamp(1rem, 2vw, 1.5rem); border: 1px solid var(--nf-color-border); border-radius: .65rem; background: var(--nf-color-surface); }
form, nav { display: flex; flex-wrap: wrap; gap: .6rem; align-items: end; }
label { display: grid; gap: .35rem; }
input, select, button { min-height: 2.75rem; max-width: 100%; padding: .55rem .75rem; border: 1px solid var(--nf-color-border); border-radius: .4rem; color: var(--nf-color-text); background: var(--nf-color-surface); font: inherit; }
button { cursor: pointer; font-weight: 600; }
button:hover:not(:disabled) { background: var(--nf-color-surface-muted); }
button:disabled { cursor: default; opacity: .6; }
input:focus-visible, select:focus-visible, button:focus-visible, .table-scroll:focus-visible { outline: 2px solid var(--nf-color-primary); outline-offset: 2px; }
.table-scroll { max-width: 100%; overflow-x: auto; }
table { width: 100%; min-width: 38rem; border-collapse: collapse; }
td, th { padding: .75rem; border-bottom: 1px solid var(--nf-color-border); text-align: left; vertical-align: top; }
small { color: var(--nf-color-text-muted); overflow-wrap: anywhere; }
td button { margin: .15rem; }
nav { align-items: center; }
nav span { padding: 0 .25rem; }
.invite-form label { flex: 1 1 14rem; }
ul { margin: 0; padding-left: 1.5rem; }
li { margin: .35rem 0; overflow-wrap: anywhere; }
@media (max-width: 36rem) { .admin header button { width: 100%; } form > input, form > select, form > button, .invite-form label { width: 100%; } }
</style>
