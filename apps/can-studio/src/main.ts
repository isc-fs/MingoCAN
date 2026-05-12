import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

const target = document.getElementById('app');
if (target === null) {
    throw new Error('#app target not found in index.html');
}

mount(App, { target });
