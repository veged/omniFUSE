import React from 'react';
import ReactDOM from 'react-dom/client';
import { MantineProvider } from '@mantine/core';
import App from './App';
import '@mantine/core/styles.css';
import './App.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <MantineProvider defaultColorScheme="auto">
      <App />
    </MantineProvider>
  </React.StrictMode>,
);
