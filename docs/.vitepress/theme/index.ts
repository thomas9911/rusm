// .vitepress/theme/index.ts
// Extends VitePress's default theme so every doc page, the nav, sidebar,
// search and footer keep working — we only restyle them and swap the home page.
import type { Theme } from 'vitepress';
import DefaultTheme from 'vitepress/theme';
import Layout from './Layout.vue';
import './custom.css';

export default {
  extends: DefaultTheme,
  Layout,
} satisfies Theme;
