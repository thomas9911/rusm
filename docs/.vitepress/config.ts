import { defineConfig } from 'vitepress';

export default defineConfig({
  title: 'RUSM',
  description: 'An Erlang-inspired WebAssembly runtime in Rust.',
  cleanUrls: true,
  themeConfig: {
    nav: [
      { text: 'Vision', link: '/00-vision' },
      { text: 'Architecture', link: '/01-architecture' },
      { text: 'Roadmap', link: '/02-roadmap' },
    ],
    // One front-to-back line: why → how → the plan, then the concepts and
    // subsystems in roadmap-phase order, then how to contribute, reference, and
    // the per-phase log.
    sidebar: [
      {
        text: 'Introduction',
        items: [
          { text: 'Vision — why RUSM', link: '/00-vision' },
          { text: 'Architecture', link: '/01-architecture' },
          { text: 'Roadmap', link: '/02-roadmap' },
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
          { text: 'Permissions & sandboxing', link: '/concepts/permissions-and-sandboxing' },
          { text: 'Distributed nodes', link: '/concepts/distributed-nodes' },
          { text: 'Live attach', link: '/concepts/live-attach' },
        ],
      },
      {
        // Feature/subsystem references, also in phase order (0 → 6 → 9).
        text: 'Subsystems',
        items: [
          { text: 'Benchmark & dashboard', link: '/03-benchmark-dashboard' },
          { text: 'Host ABI', link: '/05-host-abi' },
          { text: 'Distributed model', link: '/04-distributed-model' },
        ],
      },
      {
        text: 'Contributing',
        items: [{ text: 'Development', link: '/06-development' }],
      },
      {
        text: 'Reference',
        items: [
          { text: 'Glossary', link: '/07-glossary' },
          { text: 'RUSM vs Lunatic', link: '/lunatic-comparison' },
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
    ],
    search: { provider: 'local' },
    socialLinks: [{ icon: 'github', link: 'https://github.com/archan937/rusm' }],
    footer: {
      message: 'MIT licensed',
      copyright: '© Paul Engel',
    },
  },
});
