<script setup>
import { ref, onMounted, nextTick, computed, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import { readTextFile } from '@tauri-apps/plugin-fs'

const musicList     = ref([])
const audioRef      = ref(null)
const isScanning    = ref(false)
const currentTrack  = ref(null)
const isPlaying     = ref(false)
const currentTime   = ref(0)
const totalDuration = ref(0)
const volume        = ref(1)
const isMuted       = ref(false)

// 播放模式：0=顺序  1=单曲循环  2=随机
const playMode = ref(0)
const MODES = [
  { icon: '⇄', label: '顺序' },
  { icon: '⟳', label: '单曲循环' },
  { icon: '⇅', label: '随机' },
]
const cycleMode = () => { playMode.value = (playMode.value + 1) % 3 }

// ── 歌词 ──
const lrcLines = ref([])   // [{ time: 秒, text: '歌词行' }]
const lrcIndex = ref(-1)   // 当前高亮行
const lrcRef   = ref(null) // 歌词容器 DOM ref

const parseLrc = (text) => {
  const lines = []
  const timeReg = /\[(\d{1,2}):(\d{2})(?:[.:](\d{1,3}))?\]/g
  for (const raw of text.split('\n')) {
    const times = []; let m; timeReg.lastIndex = 0
    while ((m = timeReg.exec(raw)) !== null) {
      const secs = parseInt(m[1]) * 60 + parseInt(m[2]) + (m[3] ? parseInt(m[3].padEnd(3,'0')) / 1000 : 0)
      times.push(secs)
    }
    const lyric = raw.replace(/\[\d{1,2}:\d{2}(?:[.:]\d{1,3})?\]/g, '').trim()
    if (times.length && lyric)
      for (const t of times) lines.push({ time: t, text: lyric })
  }
  return lines.sort((a, b) => a.time - b.time)
}

const loadLrc = async (audioPath) => {
  lrcLines.value = []; lrcIndex.value = -1
  // 兼容大写扩展名（如 .MP3），同时尝试 .lrc 和 .LRC
  const base = audioPath.replace(/\\/g, '/').replace(/\.[^.]+$/, '')
  for (const ext of ['.lrc', '.LRC']) {
    try {
      const text = await readTextFile(base + ext)
      lrcLines.value = parseLrc(text)
      return
    } catch (e) {
    }
  }
}

// 播放进度变化时，更新高亮行并自动滚动
watch(() => currentTime.value, (t) => {
  if (!lrcLines.value.length) return
  let idx = -1
  for (let i = 0; i < lrcLines.value.length; i++) {
    if (lrcLines.value[i].time <= t) idx = i
    else break
  }
  if (idx !== lrcIndex.value) {
    lrcIndex.value = idx
    nextTick(() => {
      lrcRef.value?.querySelector('.lrc-active')
        ?.scrollIntoView({ behavior: 'smooth', block: 'center' })
    })
  }
})

const searchKeyword = ref('')
const isSearching   = ref(false)
const searchResults = ref([])
const canvasRef     = ref(null)

const displayList = computed(() =>
  searchKeyword.value.trim() ? searchResults.value : musicList.value
)

let audioCtx = null, analyser = null, source = null, rafId = null
let peakArr = [], peakTimer = []

const initAudioGraph = () => {
  if (audioCtx) return
  audioCtx = new (window.AudioContext || window.webkitAudioContext)()
  analyser = audioCtx.createAnalyser()
  analyser.fftSize = 256
  analyser.smoothingTimeConstant = 0.82
  source = audioCtx.createMediaElementSource(audioRef.value)
  source.connect(analyser)
  analyser.connect(audioCtx.destination)
  const bins = analyser.frequencyBinCount
  peakArr   = new Array(bins).fill(0)
  peakTimer = new Array(bins).fill(0)
}

const drawSpectrum = () => {
  const canvas = canvasRef.value
  if (!canvas || !analyser) return
  // 用 getBoundingClientRect 获取真实渲染尺寸，比 offsetWidth 更可靠
  const rect = canvas.getBoundingClientRect()
  const size = Math.round(rect.width) || 300
  canvas.width = size; canvas.height = size
  const ctx = canvas.getContext('2d')
  const cx = size / 2, cy = size / 2

  const bins = analyser.frequencyBinCount
  const data = new Uint8Array(bins)
  analyser.getByteFrequencyData(data)
  ctx.clearRect(0, 0, size, size)

  // 封面半径 89px，容器 300px → 比例 89/150 ≈ 0.593（半容器=150）
  const innerR  = size * 0.297   // = 89px when size=300（刚好封面边缘）
  const maxBarH = size * 0.20    // 最大柱高60px，有张力
  const barCount = 60
  const step = Math.floor(bins * 0.7 / barCount)

  for (let i = 0; i < barCount; i++) {
    let sum = 0
    for (let j = 0; j < step; j++) sum += data[i * step + j]
    const val  = sum / step / 255
    const barH = Math.max(val * maxBarH, 3)

    if (barH > peakArr[i]) { peakArr[i] = barH; peakTimer[i] = 0 }
    else { peakTimer[i]++; if (peakTimer[i] > 25) peakArr[i] = Math.max(3, peakArr[i] - 1.5) }

    const angle = (i / barCount) * Math.PI * 2 - Math.PI / 2
    const cos = Math.cos(angle), sin = Math.sin(angle)
    const x1 = cx + cos * (innerR + 1)
    const y1 = cy + sin * (innerR + 1)
    const x2 = cx + cos * (innerR + barH)
    const y2 = cy + sin * (innerR + barH)

    // 白色，亮度随音量 0.3~1.0
    const alpha = 0.3 + val * 0.7
    ctx.beginPath()
    ctx.strokeStyle = `rgba(255,255,255,${alpha.toFixed(2)})`
    ctx.lineWidth = (2 * Math.PI * innerR / barCount) * 0.5
    ctx.lineCap = 'round'
    ctx.moveTo(x1, y1)
    ctx.lineTo(x2, y2)
    ctx.stroke()

    // 峰值亮点（发光白点）
    if (peakArr[i] > 5) {
      const px = cx + cos * (innerR + peakArr[i] + 2)
      const py = cy + sin * (innerR + peakArr[i] + 2)
      ctx.save()
      ctx.beginPath()
      ctx.arc(px, py, 2.5, 0, Math.PI * 2)
      ctx.fillStyle = 'rgba(255,255,255,0.95)'
      ctx.shadowColor = 'rgba(255,255,255,0.9)'
      ctx.shadowBlur = 8
      ctx.fill()
      ctx.restore()
    }
  }

  // 封面外圈淡光晕
  const glow = ctx.createRadialGradient(cx, cy, innerR, cx, cy, innerR + 10)
  glow.addColorStop(0, 'rgba(255,255,255,0.15)')
  glow.addColorStop(1, 'rgba(255,255,255,0)')
  ctx.beginPath()
  ctx.arc(cx, cy, innerR + 5, 0, Math.PI * 2)
  ctx.strokeStyle = glow
  ctx.lineWidth = 10
  ctx.stroke()

  rafId = requestAnimationFrame(drawSpectrum)
}


const stopSpectrum = () => {
  if (rafId) { cancelAnimationFrame(rafId); rafId = null }
  const canvas = canvasRef.value
  if (canvas) canvas.getContext('2d').clearRect(0, 0, canvas.width, canvas.height)
}

onMounted(async () => {
  await listen('music-found', (e) => musicList.value.push(e.payload))
  await listen('scan-finished', () => { isScanning.value = false })
  const a = audioRef.value
  a.addEventListener('timeupdate',     () => { currentTime.value = a.currentTime })
  a.addEventListener('loadedmetadata', () => { totalDuration.value = a.duration || 0 })
  a.addEventListener('play', () => {
    isPlaying.value = true
    if (!audioCtx) initAudioGraph()
    if (audioCtx.state === 'suspended') audioCtx.resume()
    if (!rafId) drawSpectrum()
  })
  a.addEventListener('pause', () => { isPlaying.value = false; stopSpectrum() })
  a.addEventListener('ended', () => { isPlaying.value = false; stopSpectrum(); handleEnded() })
  a.addEventListener('volumechange', () => { volume.value = a.volume; isMuted.value = a.muted })
})

const selectAndScan = async () => {
  const selected = await open({ directory: true, multiple: false })
  if (!selected) return
  musicList.value = []; searchKeyword.value = ''; isScanning.value = true
  try { await invoke('start_scan_process', { dirPath: selected }) }
  catch (e) { console.error(e); isScanning.value = false }
}

let searchTimer = null
const onSearchInput = () => {
  clearTimeout(searchTimer)
  if (!searchKeyword.value.trim()) { searchResults.value = []; return }
  searchTimer = setTimeout(async () => {
    isSearching.value = true
    try { searchResults.value = await invoke('search_songs', { keyword: searchKeyword.value.trim() }) }
    finally { isSearching.value = false }
  }, 300)
}

const buildUrl = (p) => 'http://asset.localhost/' + p.replace(/\\/g, '/')

const playMusic = async (item) => {
  if (!item?.path || !audioRef.value) return
  currentTrack.value = item; currentTime.value = 0; totalDuration.value = 0
  stopSpectrum()
  await loadLrc(item.path)
  try {
    audioRef.value.pause()
    audioRef.value.src = buildUrl(item.path)
    await nextTick(); audioRef.value.load()
    await audioRef.value.play()
  } catch (e) { if (e.name !== 'AbortError') console.error(e.message) }
}

const togglePlay = async () => {
  if (!audioRef.value) return
  if (isPlaying.value) { audioRef.value.pause() }
  else if (!currentTrack.value && displayList.value.length) { await playMusic(displayList.value[0]) }
  else { await audioRef.value.play().catch(() => {}) }
}

const playNext = async () => {
  const l = displayList.value; if (!l.length) return
  const i = l.findIndex(x => x.path === currentTrack.value?.path)
  await playMusic(l[(i + 1) % l.length])
}

const handleEnded = async () => {
  if (playMode.value === 1) {
    // 单曲循环
    audioRef.value.currentTime = 0
    await audioRef.value.play().catch(() => {})
  } else if (playMode.value === 2) {
    // 随机：排除当前曲目
    const l = displayList.value; if (!l.length) return
    let idx
    do { idx = Math.floor(Math.random() * l.length) }
    while (l.length > 1 && l[idx].path === currentTrack.value?.path)
    await playMusic(l[idx])
  } else {
    await playNext()
  }
}
const playPrev = async () => {
  const l = displayList.value; if (!l.length) return
  const i = l.findIndex(x => x.path === currentTrack.value?.path)
  await playMusic(l[(i - 1 + l.length) % l.length])
}

const seek = (e) => {
  if (!audioRef.value || !totalDuration.value) return
  const r = e.currentTarget.getBoundingClientRect()
  audioRef.value.currentTime = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * totalDuration.value
}
const progressPct = computed(() =>
  totalDuration.value ? (currentTime.value / totalDuration.value) * 100 : 0
)
const setVolume = (e) => {
  if (!audioRef.value) return
  const r = e.currentTarget.getBoundingClientRect()
  const v = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width))
  audioRef.value.volume = v; audioRef.value.muted = v === 0
}
const toggleMute = () => { if (audioRef.value) audioRef.value.muted = !audioRef.value.muted }

const displayName = (item) => item?.title || item?.name?.replace(/\.[^.]+$/, '') || ''
const fmt = (s) => (!s || isNaN(s)) ? '0:00' : `${Math.floor(s/60)}:${String(Math.floor(s%60)).padStart(2,'0')}`
const volIcon = computed(() => isMuted.value || volume.value === 0 ? '🔇' : volume.value < 0.5 ? '🔉' : '🔊')
const defaultCover = `data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Crect width='100' height='100' fill='%23f0f0f5'/%3E%3Ccircle cx='50' cy='50' r='24' fill='%23dde'/%3E%3Ccircle cx='50' cy='50' r='9' fill='%23bbc'/%3E%3Ccircle cx='50' cy='50' r='3' fill='%23eef'/%3E%3C/svg%3E`
</script>

<template>
  <div class="app">
    <!-- 全屏模糊背景 -->
    <div class="bg-layer">
      <img :src="currentTrack?.cover || defaultCover" class="bg-img" alt=""/>
      <div class="bg-overlay"/>
    </div>

    <div class="main-layout">

      <!-- 左侧：封面圆形频谱 + 歌曲信息 + 歌词 -->
      <div class="left-panel">

        <!-- 封面 + 圆形频谱 -->
        <div class="cover-container">
          <canvas ref="canvasRef" class="spectrum-canvas"/>
          <div class="cover-wrap" :class="{ spin: isPlaying }">
            <img :src="currentTrack?.cover || defaultCover" class="cover-img" alt=""/>
          </div>
        </div>

        <!-- 歌曲信息 -->
        <div class="track-info">
          <p class="track-title">{{ currentTrack ? displayName(currentTrack) : '未在播放' }}</p>
          <p class="track-artist">{{ currentTrack?.artist || '—' }}</p>
          <p class="track-album" v-if="currentTrack?.album">{{ currentTrack.album }}</p>
        </div>

        <!-- 歌词 -->
        <div class="lrc-panel" ref="lrcRef" v-if="lrcLines.length">
          <p
            v-for="(line, i) in lrcLines" :key="i"
            class="lrc-line"
            :class="{ 'lrc-active': i === lrcIndex }"
          >{{ line.text }}</p>
        </div>
        <div class="lrc-empty" v-else-if="currentTrack">暂无歌词</div>

      </div>

      <!-- 右侧：搜索 + 歌曲列表 -->
      <div class="right-panel">

        <!-- 顶栏 -->
        <div class="topbar">
          <div class="logo">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
              <circle cx="12" cy="12" r="10" stroke="rgba(255,255,255,0.9)" stroke-width="2"/>
              <circle cx="12" cy="12" r="4" fill="rgba(255,255,255,0.9)"/>
              <path d="M12 2v4M12 18v4M2 12h4M18 12h4" stroke="rgba(255,255,255,0.9)" stroke-width="1.5" stroke-linecap="round"/>
            </svg>
            <span>MusicPlayer</span>
          </div>
          <div class="search-wrap">
            <svg class="s-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/></svg>
            <input v-model="searchKeyword" @input="onSearchInput" class="s-input" placeholder="搜索歌曲、歌手或专辑…"/>
            <button v-if="searchKeyword" class="s-clear" @click="searchKeyword='';searchResults=[]">✕</button>
            <span class="s-count">{{ searchKeyword ? searchResults.length : musicList.length }} 首</span>
          </div>
        </div>

        <!-- 歌曲列表 -->
        <div class="list-area">
          <div class="list-head">
            <span class="c-num">#</span>
            <span class="c-title">歌曲</span>
            <span class="c-album">专辑</span>
            <span class="c-dur">时长</span>
          </div>
          <ul class="track-list">
            <li class="no-track" v-if="!displayList.length">
              {{ isScanning ? '正在扫描…' : searchKeyword ? '未找到相关歌曲' : '请先添加音乐文件夹' }}
            </li>
            <li
              v-for="(item, i) in displayList" :key="item.path"
              class="track-row" :class="{ active: currentTrack?.path === item.path }"
              @click="playMusic(item)"
            >
              <span class="c-num idx-cell">
                <span class="idx-n" v-if="currentTrack?.path !== item.path">{{ i+1 }}</span>
                <span class="idx-bars" v-else>
                  <i class="b" :class="{a:isPlaying}"/>
                  <i class="b" :class="{a:isPlaying}"/>
                  <i class="b" :class="{a:isPlaying}"/>
                </span>
              </span>
              <span class="c-title row-title">
                <img :src="item.cover || defaultCover" class="row-cover" alt=""/>
                <span class="row-texts">
                  <span class="rt-name">{{ displayName(item) }}</span>
                  <span class="rt-sub">{{ item.artist || '未知歌手' }}</span>
                </span>
              </span>
              <span class="c-album rt-sub">{{ item.album || '—' }}</span>
              <span class="c-dur rt-sub" style="text-align:right">{{ fmt(item.duration) }}</span>
            </li>
          </ul>
        </div>

        <!-- 播放栏 -->
        <div class="player-bar">
          <div class="pb-left">
            <button class="add-btn" @click="selectAndScan" :disabled="isScanning">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 5v14M5 12h14"/></svg>
              {{ isScanning ? '扫描中…' : '添加文件夹' }}
            </button>
            <span class="lib-stat" v-if="musicList.length">{{ musicList.length }} 首</span>
          </div>

          <div class="pb-center">
            <div class="pb-btns">
              <button class="pb-btn mode-btn" :class="{'mode-active': playMode > 0}"
                      @click="cycleMode" :title="MODES[playMode].label">
                {{ MODES[playMode].icon }}
              </button>
              <button class="pb-btn" @click="playPrev">
                <svg viewBox="0 0 24 24"><path d="M19 20L9 12l10-8v16zM5 4h2v16H5z" fill="currentColor"/></svg>
              </button>
              <button class="pb-btn pb-play" @click="togglePlay">
                <svg v-if="!isPlaying" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" fill="currentColor"/></svg>
                <svg v-else viewBox="0 0 24 24"><path d="M6 19h4V5H6v14zm8-14v14h4V5h-4z" fill="currentColor"/></svg>
              </button>
              <button class="pb-btn" @click="playNext">
                <svg viewBox="0 0 24 24"><path d="M5 4l10 8-10 8V4zm14 0h-2v16h2z" fill="currentColor"/></svg>
              </button>
            </div>
            <div class="pb-progress">
              <span class="pb-time">{{ fmt(currentTime) }}</span>
              <div class="prog-track" @click="seek">
                <div class="prog-fill" :style="{ width: progressPct + '%' }"/>
                <div class="prog-dot"  :style="{ left: progressPct + '%' }"/>
              </div>
              <span class="pb-time" style="text-align:right">{{ fmt(totalDuration) }}</span>
            </div>
          </div>

          <div class="pb-right">
            <span class="mode-label">{{ MODES[playMode].label }}</span>
            <button class="vol-btn" @click="toggleMute">{{ volIcon }}</button>
            <div class="vol-track" @click="setVolume">
              <div class="vol-fill" :style="{ width: (isMuted ? 0 : volume * 100) + '%' }"/>
            </div>
          </div>
        </div>

      </div>
    </div>

    <audio ref="audioRef" style="display:none" crossorigin="anonymous"/>
  </div>
</template>

<style scoped>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
button{font-family:inherit;cursor:pointer}ul{list-style:none}
:global(html),:global(body){width:100%;height:100%;overflow:hidden;margin:0;padding:0}

.app{
  position:relative;
  width:100vw;height:100vh;
  overflow:hidden;
  font-family:'PingFang SC','Microsoft YaHei','Noto Sans SC',sans-serif;
  color:#fff;
}

/* ── 背景：亮度提高到 0.6，不那么暗 ── */
.bg-layer{position:absolute;inset:0;z-index:0;overflow:hidden;}
.bg-img{
  width:100%;height:100%;object-fit:cover;
  filter:blur(70px) saturate(2.2) brightness(0.6);
  transform:scale(1.15);
  transition:opacity 1.2s ease;
}
.bg-overlay{
  position:absolute;inset:0;
  background:linear-gradient(135deg,rgba(0,0,0,0.38) 0%,rgba(0,0,0,0.22) 100%);
}

/* ── 主布局：左 360px 固定，右侧自适应 ── */
.main-layout{position:relative;z-index:1;display:flex;width:100%;height:100%;}

/* ── 左侧面板：加宽到 360px ── */
.left-panel{
  width:360px;min-width:360px;height:100%;
  display:flex;flex-direction:column;align-items:center;
  padding:28px 24px 0;gap:14px;
  background:rgba(0,0,0,0.18);
  border-right:1px solid rgba(255,255,255,0.08);
  overflow:hidden;
}

/* ── 封面容器：放大到 260px ── */
.cover-container{
  position:relative;
  width:300px;height:300px;
  flex-shrink:0;
  display:flex;align-items:center;justify-content:center;
}
.spectrum-canvas{
  position:absolute;inset:0;width:100%;height:100%;
  pointer-events:none;z-index:1;
}
.cover-wrap{
  position:relative;
  width:178px;height:178px;
  border-radius:50%;z-index:2;
  box-shadow:0 8px 40px rgba(0,0,0,0.6);
}
.cover-wrap.spin .cover-img{animation:dspin 18s linear infinite;}
@keyframes dspin{to{transform:rotate(360deg);}}
.cover-img{width:100%;height:100%;border-radius:50%;object-fit:cover;display:block;}

/* ── 歌曲信息 ── */
.track-info{display:flex;flex-direction:column;align-items:center;gap:5px;width:100%;flex-shrink:0;}
.track-title{font-size:18px;font-weight:700;color:#fff;text-align:center;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;width:100%;text-shadow:0 2px 12px rgba(0,0,0,0.4);}
.track-artist{font-size:14px;color:rgba(255,255,255,0.75);text-align:center;}
.track-album{font-size:12px;color:rgba(255,255,255,0.45);text-align:center;}

/* ── 歌词面板：字体加大，占满剩余空间 ── */
.lrc-panel{
  flex:1;min-height:0;width:100%;
  overflow-y:auto;padding:6px 0 20px;
  scrollbar-width:none;
  mask-image:linear-gradient(to bottom,transparent,black 10%,black 90%,transparent);
  -webkit-mask-image:linear-gradient(to bottom,transparent,black 10%,black 90%,transparent);
}
.lrc-panel::-webkit-scrollbar{display:none;}
.lrc-line{
  font-size:14px;
  color:rgba(255,255,255,0.38);
  text-align:center;padding:6px 8px;line-height:1.7;
  transition:all .4s ease;
}
.lrc-active{
  color:#fff !important;
  font-size:16px;font-weight:700;
  text-shadow:0 0 24px rgba(255,255,255,0.7),0 0 8px rgba(200,180,255,0.8);
}
.lrc-empty{flex:1;display:flex;align-items:center;justify-content:center;font-size:13px;color:rgba(255,255,255,0.3);}

/* ── 右侧面板 ── */
.right-panel{flex:1;min-width:0;height:100%;display:flex;flex-direction:column;overflow:hidden;}

/* 顶栏 */
.topbar{flex-shrink:0;display:flex;align-items:center;gap:12px;padding:16px 22px 12px;}
.logo{display:flex;align-items:center;gap:7px;font-size:13px;font-weight:700;color:rgba(255,255,255,0.9);letter-spacing:.04em;flex-shrink:0;}
.search-wrap{
  flex:1;display:flex;align-items:center;gap:7px;
  background:rgba(255,255,255,0.14);border:1px solid rgba(255,255,255,0.18);
  border-radius:20px;padding:0 14px;transition:all .2s;backdrop-filter:blur(10px);
}
.search-wrap:focus-within{background:rgba(255,255,255,0.22);border-color:rgba(255,255,255,0.35);}
.s-ico{width:14px;height:14px;color:rgba(255,255,255,0.5);flex-shrink:0;}
.s-input{flex:1;border:none;outline:none;font-size:13px;padding:9px 0;background:transparent;color:#fff;font-family:inherit;}
.s-input::placeholder{color:rgba(255,255,255,0.35);}
.s-clear{background:none;border:none;color:rgba(255,255,255,0.4);font-size:11px;padding:2px 4px;}
.s-clear:hover{color:rgba(255,255,255,0.8);}
.s-count{font-size:11px;color:rgba(255,255,255,0.35);white-space:nowrap;}

/* 列表 */
.list-area{flex:1;min-height:0;display:flex;flex-direction:column;overflow:hidden;padding:0 22px;}
.list-head{
  display:grid;grid-template-columns:44px 1fr 160px 56px;
  padding:0 10px 10px;font-size:11px;
  color:rgba(255,255,255,0.38);letter-spacing:.07em;text-transform:uppercase;
  border-bottom:1px solid rgba(255,255,255,0.1);flex-shrink:0;
}
.track-list{flex:1;overflow-y:auto;padding-bottom:4px;scrollbar-width:thin;scrollbar-color:rgba(255,255,255,0.15) transparent;}
.track-list::-webkit-scrollbar{width:4px;}
.track-list::-webkit-scrollbar-thumb{background:rgba(255,255,255,0.15);border-radius:2px;}
.no-track{display:flex;align-items:center;justify-content:center;height:120px;color:rgba(255,255,255,0.3);font-size:14px;}
.track-row{
  display:grid;grid-template-columns:44px 1fr 160px 56px;
  align-items:center;padding:7px 10px;border-radius:10px;
  cursor:pointer;transition:background .15s;
}
.track-row:hover{background:rgba(255,255,255,0.1);}
.track-row.active{background:rgba(255,255,255,0.16);}
.track-row.active .rt-name{color:#fff;font-weight:600;}
.idx-cell{display:flex;align-items:center;justify-content:center;}
.idx-n{font-size:13px;color:rgba(255,255,255,0.3);}
.idx-bars{display:flex;align-items:flex-end;gap:2px;height:15px;}
.b{display:inline-block;width:3px;background:#fff;border-radius:2px;height:5px;}
.b.a:nth-child(1){animation:bar .8s ease-in-out infinite;}
.b.a:nth-child(2){animation:bar .8s ease-in-out .2s infinite;}
.b.a:nth-child(3){animation:bar .8s ease-in-out .4s infinite;}
@keyframes bar{0%,100%{height:4px}50%{height:14px}}
.row-title{display:flex;align-items:center;gap:10px;overflow:hidden;}
.row-cover{width:40px;height:40px;border-radius:7px;object-fit:cover;flex-shrink:0;background:rgba(255,255,255,0.08);box-shadow:0 2px 8px rgba(0,0,0,0.3);}
.row-texts{display:flex;flex-direction:column;gap:2px;overflow:hidden;}
.rt-name{font-size:14px;font-weight:500;color:rgba(255,255,255,0.88);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}
.rt-sub{font-size:12px;color:rgba(255,255,255,0.42);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}

/* 播放栏 */
.player-bar{
  flex-shrink:0;height:76px;
  display:grid;grid-template-columns:200px 1fr 200px;
  align-items:center;padding:0 22px;gap:12px;
  background:rgba(0,0,0,0.35);
  backdrop-filter:blur(24px);-webkit-backdrop-filter:blur(24px);
  border-top:1px solid rgba(255,255,255,0.1);
}
.pb-left{display:flex;align-items:center;gap:10px;}
.add-btn{
  display:flex;align-items:center;gap:5px;
  padding:7px 14px;border-radius:20px;
  border:1px solid rgba(255,255,255,0.25);
  background:rgba(255,255,255,0.1);
  color:rgba(255,255,255,0.85);font-size:12px;transition:all .2s;
}
.add-btn:hover:not(:disabled){background:rgba(255,255,255,0.2);}
.add-btn:disabled{opacity:.4;cursor:not-allowed;}
.lib-stat{font-size:12px;color:rgba(255,255,255,0.38);}
.pb-center{display:flex;flex-direction:column;align-items:center;gap:6px;}
.pb-btns{display:flex;align-items:center;gap:5px;}
.pb-btn{
  background:none;border:none;color:rgba(255,255,255,0.65);
  width:32px;height:32px;display:flex;align-items:center;justify-content:center;
  border-radius:50%;transition:all .15s;padding:0;font-size:16px;
}
.pb-btn svg{width:15px;height:15px;}
.pb-btn:hover{color:#fff;background:rgba(255,255,255,0.15);}
.pb-play{
  width:42px;height:42px;
  background:rgba(255,255,255,0.95) !important;
  color:#1a1a2e !important;
  box-shadow:0 4px 18px rgba(0,0,0,0.5);
}
.pb-play svg{width:20px;height:20px;}
.pb-play:hover{background:#fff !important;transform:scale(1.06);}
.mode-btn{font-size:15px;}
.mode-active{color:#fff !important;}
.pb-progress{display:flex;align-items:center;gap:8px;width:100%;}
.pb-time{font-size:11px;color:rgba(255,255,255,0.4);min-width:30px;}
.prog-track{flex:1;height:3px;background:rgba(255,255,255,0.2);border-radius:2px;cursor:pointer;position:relative;}
.prog-track:hover .prog-dot{opacity:1;}
.prog-fill{height:100%;background:rgba(255,255,255,0.9);border-radius:2px;pointer-events:none;transition:width .1s linear;}
.prog-dot{position:absolute;top:50%;transform:translate(-50%,-50%);width:12px;height:12px;border-radius:50%;background:#fff;opacity:0;transition:opacity .15s;pointer-events:none;}
.pb-right{display:flex;align-items:center;gap:8px;justify-content:flex-end;}
.mode-label{font-size:11px;color:rgba(255,255,255,0.35);white-space:nowrap;min-width:44px;text-align:right;}
.vol-btn{background:none;border:none;font-size:15px;padding:3px;line-height:1;color:rgba(255,255,255,0.65);}
.vol-track{width:72px;height:3px;background:rgba(255,255,255,0.2);border-radius:2px;cursor:pointer;}
.vol-fill{height:100%;background:rgba(255,255,255,0.85);border-radius:2px;pointer-events:none;transition:width .1s;}
</style>