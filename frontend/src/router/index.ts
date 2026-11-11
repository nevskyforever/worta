import { createRouter, createWebHistory } from '@ionic/vue-router'
import { currentPlatform } from '@/platform/runtime'
import { adminSession } from '@/api/admin'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: '/admin/login', name: 'admin-login', component: () => import('@/pages/AdminLoginPage.vue'), meta: { admin: true, title: 'Администрирование' } },
    { path: '/admin', name: 'admin', component: () => import('@/pages/AdminPage.vue'), meta: { admin: true, requiresAdmin: true, title: 'Администрирование' } },
    { path: '/invite', name: 'invite', component: () => import('@/pages/InvitationPage.vue'), meta: { standalone: true, title: 'Приглашение' } },
    { path: '/login', name: 'web-login', component: () => import('@/pages/WebAccountPage.vue'), meta: { standalone: true, webAccount: true, title: 'Вход' } },
    { path: '/cloud', name: 'web-workspace', component: () => import('@/pages/WebAccountPage.vue'), meta: { standalone: true, webAccount: true, title: 'Облачные проекты' } },
    { path: '/', redirect: () => currentPlatform() === 'web' ? '/login' : '/projects' },
    {
      path: '/projects',
      name: 'projects',
      component: () => import('@/pages/ProjectsPage.vue'),
      meta: { title: 'Проекты' },
    },
    {
      path: '/projects/:projectId',
      name: 'project-detail',
      component: () => import('@/pages/ProjectDetailPage.vue'),
      props: true,
      meta: { title: 'Проект' },
    },
    {
      path: '/projects/:projectId/stages/:stageId',
      name: 'stage-detail',
      component: () => import('@/pages/ProjectDetailPage.vue'),
      props: true,
      meta: { title: 'Источники' },
    },
    {
      path: '/projects/:projectId/notes',
      name: 'project-notes',
      component: () => import('@/pages/NotesPage.vue'),
      props: true,
      meta: { title: 'Заметки проекта' },
    },
    { path: '/projects/:projectId/document', name: 'document', component: () => import('@/pages/DocumentPage.vue'), meta: { title: 'Текст' } },
    { path: '/projects/:projectId/stages/:stageId/document', name: 'stage-document', component: () => import('@/pages/DocumentPage.vue'), meta: { title: 'Текст' } },
    { path: '/texts', name: 'texts', component: () => import('@/pages/TextsPage.vue'), meta: { title: 'Тексты' } },
    {
      path: '/maps', name: 'maps',
      component: () => import('@/pages/ProjectResourceHubPage.vue'),
      meta: { title: 'Карты проектов', resourceView: 'mindmap' },
    },
    {
      path: '/maps/:projectId', name: 'global-project-map',
      component: () => import('@/pages/NotesPage.vue'), props: true,
      meta: { title: 'Карта проекта', resourceView: 'mindmap', resourceHub: 'maps' },
    },
    {
      path: '/notes', name: 'notes',
      component: () => import('@/pages/ProjectResourceHubPage.vue'),
      meta: { title: 'Заметки проектов', resourceView: 'notes' },
    },
    {
      path: '/notes/:projectId', name: 'global-project-notes',
      component: () => import('@/pages/NotesPage.vue'), props: true,
      meta: { title: 'Заметки проекта', resourceView: 'notes', resourceHub: 'notes' },
    },
    {
      path: '/game',
      name: 'game',
      component: () => import('@/pages/GamePage.vue'),
      meta: { title: 'Игровой режим' },
    },
    {
      path: '/integrations',
      name: 'integrations',
      component: () => import('@/pages/IntegrationsPage.vue'),
      meta: { title: 'Синхронизация' },
    },
    {
      path: '/help',
      name: 'help',
      component: () => import('@/pages/HelpPage.vue'),
      meta: { title: 'Помощь' },
    },
    {
      path: '/settings',
      name: 'settings',
      component: () => import('@/pages/SettingsPage.vue'),
      meta: { title: 'Настройки' },
    },
    { path: '/:pathMatch(.*)*', redirect: '/projects' },
  ],
})

router.beforeEach(async (to) => {
  if (to.meta.requiresAdmin && !adminSession.active()) return { name: 'admin-login' }
  if (currentPlatform() === 'web' && !to.meta.admin && !to.meta.standalone) return { name: 'web-login' }
  if (to.meta.webAccount) {
    if (currentPlatform() !== 'web') return { name: 'projects' }
    const { webAccountSession } = await import('@/platform/web/webAccountSession')
    const target = webAccountSession().status === 'unlocked' ? 'web-workspace' : 'web-login'
    if (to.name !== target) return { name: target }
  }
  return true
})

router.afterEach((route) => {
  const pageTitle = typeof route.meta.title === 'string' ? route.meta.title : ''
  const product = currentPlatform() === 'web' ? 'worta' : 'nfprogress'
  document.title = pageTitle ? `${pageTitle} · ${product}` : product
})

export default router
