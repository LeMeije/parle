import React from 'react';
import ReactDOM from 'react-dom/client';
import './theme.css';
import App from './App';
import Hud from './Hud';

const isHud = window.location.hash.startsWith('#/hud');
document.body.dataset.window = isHud ? 'hud' : 'main';

// Surface fatal frontend errors instead of a silent blank window.
function dumpError(msg: string) {
  const el = document.createElement('pre');
  el.style.cssText = 'padding:16px;color:#d5382f;font-size:12px;white-space:pre-wrap;user-select:text';
  el.textContent = `Parle UI error:\n${msg}`;
  document.body.appendChild(el);
}
window.addEventListener('error', (e) => dumpError(String(e.error?.stack ?? e.message)));
window.addEventListener('unhandledrejection', (e) => dumpError(`Unhandled rejection: ${String(e.reason)}`));

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>{isHud ? <Hud /> : <App />}</React.StrictMode>,
);
