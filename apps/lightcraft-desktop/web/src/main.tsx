import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { applyBootTheme } from './bootTheme';
import App from './App';
import { LegalDocumentViewerHost } from './legal';
import { installWebErrorLogging } from './logs';

installWebErrorLogging();

const root = document.getElementById('root');
if (!root) throw new Error('Ember root element is missing');
const mount = root;
// App imports statically so its CSS ships in the entry stylesheet: a lazily inserted sheet left WebKit's <body>
// with its default margin (RC 38851848989). The theme store is created on first render, after the boot theme.
void applyBootTheme().then(() => { createRoot(mount).render(<StrictMode><App /><LegalDocumentViewerHost /></StrictMode>); });
