import React from 'react';
import ReactDOM from 'react-dom/client';
import './theme.css';
import App from './App';
import Hud from './Hud';

const isHud = window.location.hash.startsWith('#/hud');
document.body.dataset.window = isHud ? 'hud' : 'main';

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>{isHud ? <Hud /> : <App />}</React.StrictMode>,
);
