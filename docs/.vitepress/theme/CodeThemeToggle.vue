<script setup lang="ts">
// A nav-bar control that swaps the code-block syntax theme at runtime. Each block
// already carries all four families as `--shiki-<key>` CSS vars (see config.ts); this
// just flips `data-code-theme` on <html>, which custom.css uses to remap the live pair
// and the block background. The choice is persisted to localStorage and restored before
// paint by the inline head script in config.ts, so there's no flash on reload.
import { onBeforeUnmount, onMounted, ref } from 'vue';

const THEMES = [
  { id: 'rosepine', label: 'Rosé Pine', dot: '#c4a7e7' },
  { id: 'catppuccin', label: 'Catppuccin', dot: '#cba6f7' },
  { id: 'vitesse', label: 'Vitesse', dot: '#4d9375' },
  { id: 'one', label: 'One', dot: '#61afef' },
] as const;

const STORAGE_KEY = 'rusm-code-theme';
const current = ref<string>('rosepine');
const open = ref(false);
const root = ref<HTMLElement | null>(null);

function activeLabel() {
  return THEMES.find((t) => t.id === current.value)?.label ?? 'Rosé Pine';
}

function select(id: string) {
  current.value = id;
  document.documentElement.dataset.codeTheme = id;
  try {
    localStorage.setItem(STORAGE_KEY, id);
  } catch {
    /* private mode — selection still applies for this session */
  }
  open.value = false;
}

function onClickOutside(e: MouseEvent) {
  if (root.value && !root.value.contains(e.target as Node)) open.value = false;
}

onMounted(() => {
  current.value = document.documentElement.dataset.codeTheme || 'rosepine';
  document.addEventListener('click', onClickOutside);
});
onBeforeUnmount(() => document.removeEventListener('click', onClickOutside));
</script>

<template>
  <div ref="root" class="code-theme">
    <button
      class="code-theme__btn"
      type="button"
      :aria-expanded="open"
      aria-label="Change code theme"
      @click="open = !open"
    >
      <span class="code-theme__dot" :style="{ background: THEMES.find((t) => t.id === current)?.dot }" />
      {{ activeLabel() }}
    </button>
    <div v-if="open" class="code-theme__menu" role="menu">
      <button
        v-for="t in THEMES"
        :key="t.id"
        class="code-theme__item"
        :class="{ 'is-active': t.id === current }"
        type="button"
        role="menuitemradio"
        :aria-checked="t.id === current"
        @click="select(t.id)"
      >
        <span class="code-theme__dot" :style="{ background: t.dot }" />
        {{ t.label }}
        <span class="code-theme__check">✓</span>
      </button>
    </div>
  </div>
</template>
