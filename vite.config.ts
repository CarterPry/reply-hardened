import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5174,
    strictPort: false,
    watch: {
      ignored: ['**/node_modules/**', '**/src-tauri/target/**'],
    },
  },
});
