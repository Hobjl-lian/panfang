import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import './index.css'

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
          background: '#F5F6F9',
          color: '#0C0D0E',
          fontFamily: "-apple-system, 'PingFang SC', sans-serif",
        }}
      >
        <div style={{ fontSize: 16, fontWeight: 600 }}>页面出了点问题</div>
        <div style={{ fontSize: 13, color: 'rgba(12,13,14,.6)', textAlign: 'center', lineHeight: 1.7 }}>
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
            fontSize: 14,
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
