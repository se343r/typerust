# Attribution

What this port carries from elsewhere, and under what terms. Everything not listed here is original to
Typerust and is MIT (`LICENSE`); the notices of the two projects it ports are reproduced verbatim in
`NOTICE`.

Typerust is **not affiliated with the authors of either upstream** — TypeR and TyperTools are Adobe
Photoshop panels written by other people, and this is a separate project that ports their behaviour to
PhotoCraft's own type engine. It is a port, not a clean-room reimplementation: the material below is
reused under its MIT terms, as those terms allow.

## Carried over from TypeR

| What | Title | Author | Source | Licence |
|---|---|---|---|---|
| The material this port reuses: interface strings, the style-library JSON shape, the geometry constants and the pane layout | TypeR 2.5.0 | © 2017 Swirt, with ScanR and Sakushi | https://github.com/ScanR/TypeR at `342be99` | MIT |

`NOTICE` carries that notice verbatim, as the MIT terms require, so the port can live in a tree of its
own.

What is reused is carried over **as each part of TypeR is built**: the port began life as this
project's TyperTools port retargeted to TypeR's own behaviour, so the tree holds that shared
groundwork, and TypeR's own strings, geometry and layout arrive with the features they belong to. TypeR
is itself a descendant of TyperTools, which is why both rows are here.

## Carried over from TyperTools

| What | Title | Author | Source | Licence |
|---|---|---|---|---|
| The groundwork TypeR inherited, which this port reached first: the three-pane layout, the style-library format, script parsing and the Create / Insert / Align workflow | TyperTools 1.4.9 | © 2017 Swirt | https://github.com/Swirt/typertools-src | MIT |

`NOTICE` carries that notice verbatim too. It is the `LICENSE.md` at the root of the `typertools-src`
fork this port was developed in, unmodified; both upstreams ship the same MIT text, © 2017 Swirt.

## Deliberately not carried over

| Path | Why not |
|---|---|
| `app_src/lib/jam/*` | GPL v3+, © 2011–2016 Michel MARIANI. Taking it would relicense this port under the GPL, and it is ExtendScript helper code (`.jsxinc`) that a plug-in talking to PhotoCraft's own type engine has no use for. |
| `app_src/lib/CSInterface.js` | Adobe Systems, proprietary and not redistributable. There is no CEP host here to talk to. |
| `app_src/lib/topcoat/*` | Apache-2.0, © 2012 Adobe Systems Inc. — a CSS widget kit for a browser panel; the host draws this panel's widgets, so there is nothing to style. Its bundled fonts are not used. |
| `icons/*.png` | TypeR's CEP panel icons. PhotoCraft draws the panel row itself; it has no per-plug-in icon surface. |

The Markdown and `.pcraft` files this port reads and writes are its own; the Photoshop-facing formats
it interoperates with (the `.json` style library, PSD text) are handled by PhotoCraft's engine, not by
anything copied from TypeR.
