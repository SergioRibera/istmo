// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://istmo.dev',
  integrations: [
    starlight({
      title: 'Istmo',
      description:
        'Write your mobile app in Rust. Reach every platform without giving up native APIs, native performance, or native feel.',
      logo: {
        src: './src/assets/logo.svg',
        replacesTitle: false,
      },
      social: {
        github: 'https://github.com/sergioribera/istmo',
      },
      defaultLocale: 'root',
      locales: {
        root: { label: 'English', lang: 'en' },
        es: { label: 'Español', lang: 'es' },
      },
      sidebar: [
        {
          label: 'Getting started',
          translations: { es: 'Primeros pasos' },
          autogenerate: { directory: 'getting-started' },
        },
        {
          label: 'Concepts',
          translations: { es: 'Conceptos' },
          autogenerate: { directory: 'concepts' },
        },
        {
          label: 'Writing plugins',
          translations: { es: 'Escribir plugins' },
          autogenerate: { directory: 'writing-plugins' },
        },
        {
          label: 'Official plugins',
          translations: { es: 'Plugins oficiales' },
          autogenerate: { directory: 'plugins' },
        },
        {
          label: 'Build scripts',
          translations: { es: 'Build scripts' },
          autogenerate: { directory: 'build-scripts' },
        },
        {
          label: 'Advanced',
          translations: { es: 'Avanzado' },
          autogenerate: { directory: 'advanced' },
        },
      ],
      editLink: {
        baseUrl:
          'https://github.com/sergioribera/istmo/edit/main/docs/',
      },
      lastUpdated: true,
      pagination: true,
      customCss: ['./src/styles/theme.css'],
    }),
  ],
});
