import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import './index.css'
import './theme.css'
import './type.css'
import { applyTheme, readThemeMode } from './theme'
import { applyFontSize, readFontSizeMode } from './font'

// 必须在首帧渲染前落到 <html> 上：晚一步就会先闪一帧浅色（深色系统下尤其刺眼）
applyTheme(readThemeMode())
// 字号档位同理：首帧前落盘，否则会先闪一下标准字号
applyFontSize(readFontSizeMode())

/**
 * 渲染期异常兜底：React 会卸载整棵树 → 买家只看到白屏，只能杀进程。
 * 这里给一个能照做的出口（重新打开 = 重载页面，页面栈会退回安全位置）。
 */
class ErrorBoundary extends React.Component<{ children: React.ReactNode }, { err: Error | null }> {
  state = { err: null as Error | null }
  static getDerivedStateFromError(err: Error) {
    return { err }
  }
  componentDidCatch(err: Error) {
    console.error('[ui] 渲染异常', err)
  }
  render() {
    if (!this.state.err) return this.props.children
    return (
      <div
        style={{
          minHeight: '100vh',
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 14,
          padding: 28,
          background: 'var(--pf-page)',
          color: 'var(--pf-ink)',
          fontFamily: "-apple-system, 'PingFang SC', sans-serif",
        }}
      >
        <div style={{ fontSize: 'var(--pf-fs-16)', fontWeight: 600 }}>页面出了点问题</div>
        <div style={{ fontSize: 'var(--pf-fs-13)', color: 'var(--pf-i-60)', textAlign: 'center', lineHeight: 1.7 }}>
          点下面的按钮重新打开；还不行就退出应用再进一次。
        </div>
        <button
          onClick={() => {
            this.setState({ err: null })
            location.reload()
          }}
          style={{
            marginTop: 6,
            padding: '10px 24px',
            border: 0,
            borderRadius: 12,
            background: '#4361EE',
            color: '#fff',
            fontSize: 'var(--pf-fs-14)',
            fontWeight: 600,
          }}
        >
          重新打开
        </button>
      </div>
    )
  }
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
)
