Jekyll to Zola Migration
========================

## For Authors

### <abbr title="too long; didn't read">tl;dr</abbr>

Instead of [Jekyll], we're using [Zola]. Content committed to the canonical
repositories will be automatically converted, but authors will need to manually
update pull requests. The big changes for authors are:

- Proposals are renamed from `EIPS/eip-1234.md` to either `content/01234.md` or
  `content/01234/index.md` (if you have assets).
- Assets are stored alongside proposals, so `assets/eip-1234/foo.jpg` becomes
  `content/01234/assets/foo.jpg`.
- Creative Commons Zero link is now absolute (`/LICENSE.md`).
- Links to subsections might require minor tweaks.
- Template now lives in `docs/template.md`.

## For Readers

### <abbr title="too long; didn't read">tl;dr</abbr>

Experience should be mostly the same, with the following exceptions:

- Canonical proposal URLs change from `https://eips.ethereum.org/EIPs/eip-1234`
  to `https://eips.ethereum.org/1234/`. We've added redirects where possible.
- Asset URLs change too, no redirects.
- Atom/RSS feeds change dramatically. You'll want to update your feed readers.
- There's a search now!


[Jekyll]: https://docs.github.com/en/pages/setting-up-a-github-pages-site-with-jekyll/about-github-pages-and-jekyll
[Zola]: https://www.getzola.org/
