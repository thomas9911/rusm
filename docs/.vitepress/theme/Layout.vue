<script setup lang="ts">
// Wraps the default layout (so nav/sidebar/search/footer are untouched) and
// drops our custom home into the home page via the `home-hero-before` slot.
import DefaultTheme from 'vitepress/theme'
import { useData } from 'vitepress'
import RusmHome from './RusmHome.vue'
import CodeThemeToggle from './CodeThemeToggle.vue'

const { Layout } = DefaultTheme
const { frontmatter } = useData()
</script>

<template>
  <Layout>
    <template #home-hero-before>
      <RusmHome v-if="frontmatter.layout === 'home'" />
    </template>
    <template #nav-bar-content-after>
      <CodeThemeToggle />
    </template>
  </Layout>
</template>
