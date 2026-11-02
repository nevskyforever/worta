(() => {
  const root = document.documentElement
  const themeButton = document.querySelector('.theme-toggle')
  const menuButton = document.querySelector('.menu-toggle')
  const navigation = document.querySelector('#primary-nav')
  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)')

  function readTheme() {
    try {
      const saved = window.localStorage.getItem('worta-landing-theme')
      return saved === 'light' || saved === 'dark' ? saved : null
    } catch {
      return null
    }
  }

  function updateThemeButton(theme) {
    if (!themeButton) return
    const nextTheme = theme === 'dark' ? 'light' : 'dark'
    themeButton.setAttribute('aria-label', `Включить ${nextTheme === 'dark' ? 'тёмную' : 'светлую'} тему`)
    themeButton.querySelector('.theme-icon').textContent = theme === 'dark' ? '☼' : '◐'
  }

  const savedTheme = readTheme()
  if (savedTheme) root.dataset.theme = savedTheme
  updateThemeButton(savedTheme || (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'))

  themeButton?.addEventListener('click', () => {
    const current = root.dataset.theme || (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    const next = current === 'dark' ? 'light' : 'dark'
    root.dataset.theme = next
    try {
      window.localStorage.setItem('worta-landing-theme', next)
    } catch {
      // Theme selection still works for this page view.
    }
    updateThemeButton(next)
  })

  document.body.classList.add('js-enabled')
  if (menuButton && navigation) {
    menuButton.hidden = false
    menuButton.addEventListener('click', () => {
      const isOpen = menuButton.getAttribute('aria-expanded') === 'true'
      menuButton.setAttribute('aria-expanded', String(!isOpen))
      menuButton.querySelector('.visually-hidden').textContent = isOpen ? 'Открыть меню' : 'Закрыть меню'
      navigation.classList.toggle('menu-open', !isOpen)
    })

    navigation.querySelectorAll('a').forEach((link) => {
      link.addEventListener('click', () => {
        navigation.classList.remove('menu-open')
        menuButton.setAttribute('aria-expanded', 'false')
        menuButton.querySelector('.visually-hidden').textContent = 'Открыть меню'
      })
    })

    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && menuButton.getAttribute('aria-expanded') === 'true') {
        navigation.classList.remove('menu-open')
        menuButton.setAttribute('aria-expanded', 'false')
        menuButton.querySelector('.visually-hidden').textContent = 'Открыть меню'
        menuButton.focus()
      }
    })
  }

  const downloadNote = document.querySelector('#download-note')
  const supportedPlatforms = new Set(['windows', 'macos-arm', 'macos-intel'])
  const allowedDownloadHosts = new Set(['github.com', 'nfproject.ru', 'worta.ru', 'www.worta.ru'])
  const downloadFormats = {
    windows: { architecture: /windows[-_](?:x86_64|x64|x86-64)|win[-_]x64/i, extension: /\.exe$/i },
    'macos-arm': { architecture: /(?:mac|macos|darwin)[-_](?:arm|arm64|aarch64)/i, extension: /\.(?:dmg|zip)$/i },
    'macos-intel': { architecture: /(?:mac|macos|darwin)[-_](?:intel|x86_64|x64)/i, extension: /\.(?:dmg|zip)$/i },
  }

  function validRelease(release) {
    if (!release || typeof release !== 'object') return false
    if (!supportedPlatforms.has(release.platform)) return false
    if (!['available', 'preparing', 'unavailable'].includes(release.status)) return false
    if (release.version && (typeof release.version !== 'string' || !/^\d+\.\d+\.\d+(?:[-+][\da-z.-]+)?$/i.test(release.version))) return false
    if (release.status === 'preparing' && !release.version) return false

    if (release.status !== 'available') return release.url == null
    if (!release.version || typeof release.url !== 'string') return false

    try {
      const url = new URL(release.url)
      const format = downloadFormats[release.platform]
      const filename = decodeURIComponent(url.pathname.split('/').pop() || '')
      return url.protocol === 'https:' && !url.username && !url.password && !url.port && allowedDownloadHosts.has(url.hostname) && url.pathname.includes(release.version) && format.architecture.test(filename) && format.extension.test(filename)
    } catch {
      return false
    }
  }

  function displayProductName(text) {
    return text.replace(/\b(?:WORTA|Worta|NFProgress)\b/g, 'worta')
  }

  async function loadDesktopReleases() {
    if (!downloadNote) return

    try {
      const response = await fetch('releases.json', { cache: 'no-cache' })
      if (!response.ok) throw new Error(`HTTP ${response.status}`)

      const data = await response.json()
      if (!data || data.schema_version !== 1 || !Array.isArray(data.releases) || data.releases.length !== supportedPlatforms.size) {
        throw new Error('Invalid release list')
      }

      const releases = data.releases.filter(validRelease)
      if (releases.length !== supportedPlatforms.size || new Set(releases.map((release) => release.platform)).size !== supportedPlatforms.size) {
        throw new Error('Invalid platform list')
      }
      const uniqueReleases = new Map(releases.map((release) => [release.platform, release]))
      uniqueReleases.forEach((release, platform) => {
        const card = document.querySelector(`.download-card[data-platform="${platform}"]`)
        if (!card) return

        const version = card.querySelector('.download-version')
        const status = card.querySelector('.download-status')
        card.dataset.releaseStatus = release.status
        version.textContent = release.version ? `Версия ${release.version}` : 'Версия не подтверждена'
        status.textContent = typeof release.message === 'string'
          ? displayProductName(release.message)
          : release.status === 'preparing'
            ? `Версия ${release.version} готовится.`
            : release.status === 'unavailable'
              ? 'Временно недоступно.'
              : 'Установочный файл опубликован.'

        if (release.status !== 'available') return

        const link = document.createElement('a')
        link.className = 'download-button'
        link.href = release.url
        link.textContent = 'Скачать'
        card.append(link)
      })

      if (Array.from(uniqueReleases.values()).some((release) => release.status === 'available')) {
        downloadNote.textContent = typeof data.message === 'string'
          ? displayProductName(data.message)
          : 'Выберите сборку для своей платформы.'
      } else {
        downloadNote.textContent = typeof data.message === 'string'
          ? displayProductName(data.message)
          : 'Загрузки временно недоступны: подтверждённых установщиков worta пока нет.'
      }
    } catch {
      downloadNote.textContent = 'Не удалось загрузить список релизов. Попробуйте позже.'
    }
  }

  loadDesktopReleases()

  if (!reducedMotion.matches && 'IntersectionObserver' in window) {
    const observer = new IntersectionObserver((entries, currentObserver) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible')
          currentObserver.unobserve(entry.target)
        }
      })
    }, { threshold: 0.12 })

    document.querySelectorAll('.reveal').forEach((element) => observer.observe(element))
    document.body.classList.add('motion-ready')
  }
})()
