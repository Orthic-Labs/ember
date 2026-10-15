import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { applyBootTheme } from './bootTheme';

const root = document.getElementById('root');
if (!root) throw new Error('Ember root element is missing');
const mount = root;
// App's theme store reads the boot theme when its module loads, so import it afterwards.
void applyBootTheme()
  .then(() => import('./App'))
  .then(({ default: App }) => { createRoot(mount).render(<StrictMode><App /></StrictMode>); });
