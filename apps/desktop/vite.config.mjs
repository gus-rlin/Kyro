import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { nativeDevelopment } from './scripts/native-development.mjs';

export default defineConfig({
  plugins: [react(), nativeDevelopment(), {
    name: 'development-csp', apply: 'serve',
    transformIndexHtml: (html) => html.replace("script-src 'self'", "script-src 'self' 'unsafe-inline'"),
  }, {
    name: 'packaged-csp', apply: 'build',
    transformIndexHtml: (html) => html.replace(' ws://127.0.0.1:5174', ''),
  }],
  base: './',
  server: { host: '127.0.0.1', port: 5174, strictPort: true },
});
