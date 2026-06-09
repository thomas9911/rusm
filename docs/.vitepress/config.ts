import { defineConfig } from 'vitepress';

// One source of truth for navigation: the same grouped structure drives both the
// top nav (as dropdowns) and the sidebar (as sections), so they never diverge.
const sections = [
  {
    text: 'Get started',
    items: [
      { text: 'Overview', link: '/getting-started' },
      { text: 'Install', link: '/getting-started#install' },
      { text: 'Quick start', link: '/getting-started#quick-start' },
    ],
  },
  {
    text: 'About RUSM',
    items: [
      {
        text: 'Overview',
        items: [
          { text: 'Why RUSM?', link: '/00-vision' },
          { text: 'Features', link: '/features' },
        ],
      },
      {
        text: 'Comparisons',
        items: [
          { text: 'RUSM vs Lunatic', link: '/lunatic-comparison' },
          { text: 'How RUSM compares', link: '/comparison' },
          { text: 'Design analysis', link: '/design-analysis' },
        ],
      },
      {
        text: 'The project',
        items: [
          { text: 'Architecture', link: '/01-architecture' },
          { text: 'Roadmap', link: '/02-roadmap' },
          { text: 'Development', link: '/06-development' },
        ],
      },
    ],
  },
  {
    // Grouped into categories (see /features for the value-first map). One level of
    // nesting renders as grouped sections in both the sidebar and the nav dropdown.
    text: 'Concepts',
    items: [
      {
        text: 'The actor model',
        items: [
          { text: 'The process model', link: '/concepts/wasm-instance-as-process' },
          { text: 'Message passing', link: '/concepts/message-passing' },
          { text: 'Links & supervision', link: '/concepts/links-and-supervision' },
          { text: 'Fibers & blocking→async', link: '/concepts/fibers-and-blocking-to-async' },
          { text: 'Epoch preemption', link: '/concepts/epoch-preemption' },
          { text: 'Process management', link: '/concepts/process-management' },
        ],
      },
      {
        text: 'WebAssembly & safety',
        items: [
          { text: 'Components & the actor world', link: '/concepts/components-and-the-actor-world' },
          { text: 'Permissions & sandboxing', link: '/concepts/permissions-and-sandboxing' },
          { text: 'Guests: Rust & TypeScript', link: '/concepts/guests-rust-and-typescript' },
        ],
      },
      {
        text: 'Serving & streaming',
        items: [
          { text: 'The serving model', link: '/concepts/serving-model' },
          { text: 'Byte streams', link: '/concepts/byte-streams' },
        ],
      },
      {
        text: 'Apps & clusters',
        items: [
          { text: 'The app model', link: '/concepts/app-model' },
          { text: 'Distributed nodes', link: '/concepts/distributed-nodes' },
          { text: 'Live attach', link: '/concepts/live-attach' },
        ],
      },
    ],
  },
  {
    text: 'Reference',
    items: [
      {
        text: 'APIs & models',
        items: [
          { text: 'Host ABI', link: '/05-host-abi' },
          { text: 'Distributed model', link: '/04-distributed-model' },
          { text: 'Serving HTTP/WS/SSE', link: '/serving-http-ws-sse' },
        ],
      },
      {
        text: 'Tools & appendix',
        items: [
          { text: 'Benchmark & dashboard', link: '/03-benchmark-dashboard' },
          { text: 'Glossary', link: '/07-glossary' },
        ],
      },
    ],
  },
  {
    text: 'Phase log',
    items: [
      { text: 'Phase 0 — Foundation', link: '/phases/phase-00-foundation' },
      { text: 'Phase 1 — Process core', link: '/phases/phase-01-process-core' },
      { text: 'Phase 2 — Messaging', link: '/phases/phase-02-messaging' },
      { text: 'Phase 3 — Supervision', link: '/phases/phase-03-supervision' },
      { text: 'Phase 4 — Management', link: '/phases/phase-04-management' },
      { text: 'Phase 5 — TCP', link: '/phases/phase-05-tcp' },
      { text: 'Phase 6 — Wasm backend', link: '/phases/phase-06-wasm-backend' },
      { text: 'Phase 7 — Component hosting', link: '/phases/phase-07-components' },
      { text: 'Phase 8 — Guest ergonomics', link: '/phases/phase-08-guest-ergonomics' },
      { text: 'Phase 9 — Distributed clusters', link: '/phases/phase-09-distributed-clusters' },
      { text: 'Phase 10 — Scale & hardening', link: '/phases/phase-10-scale-hardening' },
    ],
  },
];

export default defineConfig({
  title: 'RUSM',
  description: 'An Erlang-inspired WebAssembly runtime in Rust.',
  // Served as a GitHub Pages project site at https://archan937.github.io/rusm/,
  // so every asset/link resolves under the /rusm/ subpath.
  base: '/rusm/',
  cleanUrls: true,
  // The RUSM theme's fonts (display / base / mono), loaded with preconnect for
  // performance rather than a CSS @import.
  head: [
    ['link', { rel: 'preconnect', href: 'https://fonts.googleapis.com' }],
    ['link', { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' }],
    [
      'link',
      {
        rel: 'stylesheet',
        href: 'https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,500;12..96,700;12..96,800&family=Hanken+Grotesk:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap',
      },
    ],
  ],
  themeConfig: {
    nav: sections,
    sidebar: sections,
    search: { provider: 'local' },
    socialLinks: [{ icon: 'github', link: 'https://github.com/archan937/rusm' }],
    footer: {
      message: 'MIT licensed',
      copyright: '© Paul Engel',
    },
  },
});
