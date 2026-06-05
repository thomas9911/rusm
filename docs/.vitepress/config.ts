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
    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Vision — why RUSM', link: '/00-vision' },
          { text: 'Architecture', link: '/01-architecture' },
          { text: 'Roadmap', link: '/02-roadmap' },
          { text: 'Benchmark & dashboard', link: '/03-benchmark-dashboard' },
          { text: 'Distributed model', link: '/04-distributed-model' },
          { text: 'Host ABI', link: '/05-host-abi' },
          { text: 'Development', link: '/06-development' },
          { text: 'Glossary', link: '/07-glossary' },
        ],
      },
      {
        text: 'Reference',
        items: [{ text: 'RUSM vs Lunatic', link: '/lunatic-comparison' }],
      },
      {
        text: 'Phases',
        items: [{ text: 'Phase 0 — Foundation', link: '/phases/phase-00-foundation' }],
      },
      {
        text: 'Concepts',
        items: [
          { text: 'Wasm instance as process', link: '/concepts/wasm-instance-as-process' },
          { text: 'Message passing', link: '/concepts/message-passing' },
          { text: 'Fibers & blocking→async', link: '/concepts/fibers-and-blocking-to-async' },
          { text: 'Epoch preemption', link: '/concepts/epoch-preemption' },
          { text: 'Links & supervision', link: '/concepts/links-and-supervision' },
          { text: 'Permissions & sandboxing', link: '/concepts/permissions-and-sandboxing' },
          { text: 'Distributed nodes', link: '/concepts/distributed-nodes' },
          { text: 'Live attach', link: '/concepts/live-attach' },
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
