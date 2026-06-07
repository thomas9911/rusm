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
      { text: 'Why RUSM?', link: '/00-vision' },
      { text: 'RUSM vs Lunatic', link: '/lunatic-comparison' },
      { text: 'Design analysis', link: '/design-analysis' },
      { text: 'Architecture', link: '/01-architecture' },
      { text: 'Roadmap', link: '/02-roadmap' },
      { text: 'Development', link: '/06-development' },
    ],
  },
  {
    // Ordered by the phase each concept lands in (see the roadmap).
    text: 'Concepts',
    items: [
      { text: 'The process model', link: '/concepts/wasm-instance-as-process' },
      { text: 'Message passing', link: '/concepts/message-passing' },
      { text: 'Links & supervision', link: '/concepts/links-and-supervision' },
      { text: 'Fibers & blocking→async', link: '/concepts/fibers-and-blocking-to-async' },
      { text: 'Epoch preemption', link: '/concepts/epoch-preemption' },
      { text: 'Components & the actor world', link: '/concepts/components-and-the-actor-world' },
      { text: 'Permissions & sandboxing', link: '/concepts/permissions-and-sandboxing' },
      { text: 'Byte streams', link: '/concepts/byte-streams' },
      { text: 'The app model', link: '/concepts/app-model' },
      { text: 'Distributed nodes', link: '/concepts/distributed-nodes' },
      { text: 'Live attach', link: '/concepts/live-attach' },
    ],
  },
  {
    text: 'Reference',
    items: [
      { text: 'Benchmark & dashboard', link: '/03-benchmark-dashboard' },
      { text: 'Host ABI', link: '/05-host-abi' },
      { text: 'Distributed model', link: '/04-distributed-model' },
      { text: 'Glossary', link: '/07-glossary' },
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
    ],
  },
];

export default defineConfig({
  title: 'RUSM',
  description: 'An Erlang-inspired WebAssembly runtime in Rust.',
  cleanUrls: true,
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
