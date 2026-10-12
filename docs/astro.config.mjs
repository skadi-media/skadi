// The Skadi docs (SKADI-T-0715): Astro Starlight with the Colliery theme
// starlight-aurora, in the four Diátaxis sections.
import { existsSync } from 'node:fs';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightAurora from 'starlight-aurora';

// The Pages workflow sets DOCS_SITE and DOCS_BASE. On a pull request and
// locally they are empty, and the site is at /.
const site = process.env.DOCS_SITE || undefined;
const base = process.env.DOCS_BASE || '/';

const PROJECT = 'Skadi';
const REPO = 'https://github.com/skadi-media/skadi';
const to = (path) => `${base.replace(/\/$/, '')}/${path}`;

// The API reference is written by `plissken render` (plissken.toml) and is not
// committed. Without it the site still builds, with no API pages.
const API_SIDEBAR = './src/content/docs/reference/api/_sidebar.mjs';
let apiSidebar = [];
if (existsSync(new URL(API_SIDEBAR, import.meta.url))) {
	apiSidebar = [(await import(API_SIDEBAR)).default];
} else {
	console.warn('No API reference: run `plissken render` in the repository root.');
}

// The sidebar is written out: it keeps the order of the old MkDocs nav.
const page = (slug, label) => ({ slug, label });

export default defineConfig({
	site,
	base,
	// MkDocs served each page at <path>/. These old addresses go to the new
	// pages: Astro writes a redirect page at each one. Astro does not add the
	// base path to a target, so `to` does.
	redirects: {
		'/getting-started/installation': to('tutorials/installation/'),
		'/getting-started/quickstart': to('tutorials/quickstart/'),
		'/guides/audiobooks': to('how-to/audiobooks/'),
		'/guides/operations': to('how-to/operations/'),
		'/guides/review-brief-SKADI-I-0057': to('explanation/bdd-harness/'),
		'/api': to('reference/'),
	},
	integrations: [
		starlight({
			title: PROJECT,
			description: 'A unified, Rust-native replacement for the *arr stack.',
			social: [{ icon: 'github', label: 'GitHub', href: REPO }],
			editLink: { baseUrl: `${REPO}/edit/main/docs/` },
			plugins: [starlightAurora({ hyper: true })],
			expressiveCode: { frames: { extractFileNameFromCode: false } },
			sidebar: [
				{ label: 'Home', link: '/' },
				{
					label: 'Tutorials',
					items: [
						page('tutorials/installation', 'Installation'),
						page('tutorials/quickstart', 'Quick Start'),
					],
				},
				{
					label: 'How-to guides',
					items: [
						page('how-to/audiobooks', 'Audiobooks'),
						page('how-to/operations', 'Operations runbook'),
					],
				},
				{
					label: 'Reference',
					items: [page('reference', 'Overview'), ...apiSidebar],
				},
				{
					label: 'Explanation',
					items: [page('explanation/bdd-harness', 'BDD harness gotchas')],
				},
			],
		}),
	],
});
