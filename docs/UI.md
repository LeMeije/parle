# Parle: the UI contract

Written 10/09/2026, after a round of Refine appearance work turned up a bug
class that is invisible until you measure a computed colour. PRODUCT.md says
what the UI *is*; this file says how it is *built*, and which parts will bite
you if you change them without knowing.

Everything here was measured on the running UI, not inferred from the CSS.

## The three surfaces

| Surface | Window | Markup | Styles |
|---|---|---|---|
| Main window | `main` | `src/App.tsx` + `src/views/*` | `src/App.css` |
| Overlay (HUD) | `hud` | `src/Hud.tsx` | `src/hud.css` |
| Dictation bar | inside `main` | `src/DictationBar.tsx` | `src/App.css` |

The overlay is a separate Tauri window with its own document, so it does not
inherit anything from `App.css`. A token or rule that has to apply in both
places has to be written in both files. `theme.css` is the one thing both
load, which is why every colour belongs there rather than in a component.

Tauri capabilities must list BOTH windows (`src-tauri/capabilities/default.json`);
a window missing from that list gets silent IPC failures, not errors.

## Design tokens and how the cascade works

`src/theme.css`. Palettes are attributes on `<html>`: `data-palette` is one of
`paper` (default), `pastel`, `bold`, `retro`, and `data-mode` is `light` or
`dark`. "System" is resolved in JS before the attribute is set, so the CSS
never has to deal with a third state.

The base block is `:root, [data-palette='paper']`. That double selector is
load-bearing: `:root` always matches, so every token has a value even under a
palette that only overrides a handful. `bold`, for instance, declares no
accent tokens at all and inherits Paper's. Palette blocks come later in the
file at equal specificity, so their overrides win by order.

```
:root                                   base tokens + Paper light
[data-mode='dark']                      dark overrides
[data-palette='pastel']                 + its dark variant
[data-palette='bold']                   + its dark variant
[data-palette='retro']                  + its dark variant
```

`color-scheme` is set here too, and it is not decorative: number steppers,
scrollbars and `<select>` popups are painted by the engine, not by our tokens.
Without it they stay in light dress on a dark palette, which is how the
latch-window stepper once came out black on black.

### The scoped-accent contract (read this before touching a tint)

Parle scopes `--accent` on a subtree to recolour it: the Compose root and the
dictation bar take the Refine accent for the duration of a Refine take, and
the Settings test button takes it permanently. The trap:

> **A custom property's value is substituted where it is DECLARED, not where
> it is used.**

So this, on `:root`, keeps computing from the ROOT accent no matter how far
below it `--accent` is overridden:

```css
:root { --accent-soft: color-mix(in srgb, var(--accent) 12%, transparent); }
```

Descendants inherit the already-substituted value. Measured on the Compose
Refine button before the fix, with `--accent: #ff7a59` scoped on the button
itself:

```
--accent      (scoped)     #ff7a59                       coral, correct
--accent-soft (inherited)  color-mix(… #2b5cff 12% …)    the ROOT blue
background   (computed)    color(srgb 0.168 0.360 1/.12) a 12% BLUE wash
color        (computed)    rgb(255, 122, 89)             coral
```

Coral text and a coral border over a blue wash, which reads as grey. It shipped
that way and looked like a deliberate, if drab, style choice.

The fix has two halves. Every theme block now carries the mix strength as its
own token, so the declaration can be repeated verbatim in another scope
without hardcoding a percentage:

```css
--accent-soft-mix: 12%;   /* per theme: 12 paper, 22 dark, 16 pastel, 24 pastel-dark, 16 retro */
--accent-soft: color-mix(in srgb, var(--accent) var(--accent-soft-mix), transparent);
```

and `App.css` re-declares it wherever an accent is scoped:

```css
[style*='--accent'],
.badge-refined {
  --accent-soft: color-mix(in srgb, var(--accent) var(--accent-soft-mix), transparent);
}
```

`[style*='--accent']` is a substring match on the inline style attribute, which
is deliberate: it means "anything that inline-scopes an accent gets the tint
rescoped with it", and it keeps working when a new component does the same
thing without anyone remembering this file. Two things to know about it:

- It does NOT match `style="--refine-accent: …"`. The substring `--accent`
  requires two consecutive hyphens, and `--refine-accent` has only one before
  `accent`. That is why publishing `--refine-accent` on `.app` does not drag
  the whole window into a rescope.
- Where it matches an element that scopes nothing relevant, the recomputation
  produces the same value it inherited. Harmless.

**If you add a token derived from `--accent`, add it to that rule too**, or it
will silently keep the root's colour inside every scoped subtree.

### `--wave` is the deliberate exception

`--wave` is `var(--accent)` in the base, but the **retro** palette pins it to a
fixed hue in both modes (`#e0642f` light, `#ff8c4d` dark) because that theme's
waveform is part of its cassette-deck identity, not a reflection of the accent.

So `--wave` must NOT go in the blanket rescope rule: that would repaint retro's
normal-mode waveform with the plain accent. It is rescoped only on the two
elements that actually carry a live Refine take:

```css
.compose[data-dictation-mode='refine'],
.dictation-bar[data-refine='true'] { --wave: var(--accent); }
```

Both of those elements also inline-scope `--accent`, which is what makes the
narrow rule correct rather than merely narrow.

### `--refine-accent` is window-wide, on purpose

`App.tsx` publishes the user's chosen Refine colour on `.app`:

```jsx
style={{ '--refine-accent': settings.refine.accent }}
```

Not just on the live take, because the History `refined` badge has to carry the
same colour long after the dictation that made it has finished. The badge used
to hardcode `#ff7a59`, so it ignored the colour the user picked; it now reads
`--accent: var(--refine-accent, #ff7a59)` and derives its tint from that.

## Layout primitives in Settings

Three shapes, and picking the wrong one produces a specific, recognisable mess.

**`Section`** (`settings-section` > `h2` + `.section-body`). The body is the
bordered card; children are rows.

**`Field`** (`.field`). A label-and-hint column on the left, a control cluster
on the right:

```
.field          display:flex; justify-content:space-between; gap:20px; border-bottom:1px solid
.field-label    the label and its <small> hint (hint max-width 380px)
.field-control  display:flex; gap:8px; flex: 0 0 auto
```

`.field-control` is `flex: 0 0 auto`, so **the control side never shrinks and
the label column absorbs every pixel the controls take**. Two consequences:

- A wide control cluster crushes the hint into a one-word-per-line ribbon. The
  model row hit this the moment it carried a select, a text box and a refresh
  button: the hint measured 51px wide. The fix is to cap and wrap the cluster
  (`.model-pick { max-width: 260px; flex-wrap: wrap; justify-content: flex-end }`)
  so the widest child drops to a second line instead.
- **Never put a callout inside a `Field`.** It becomes another control in that
  row and does the same thing. Callouts go after the `Field`, as a sibling, the
  way the trigger warning and the model warning both do.

Also: measure layout at the real window width. `tauri.conf.json` says 980x700,
and a 800px-wide preview pane crushes things that are perfectly fine in the
app. Two of the "bugs" found during this work were only the narrow pane.

**Full-width row** (`.sync-block`, and now `.refine-status`). For content that
cannot sit in the label/control split. Same rhythm as a `Field` so the card
reads as one list:

```css
padding: 12px 16px;
border-bottom: 1px solid var(--border);
```

The Refine result line is one of these. Getting it there needs the
`.section-body` prefix:

```css
.section-body .refine-status { margin: 0; padding: 12px 16px; border-radius: 0; border-bottom: 1px solid var(--border); }
```

because `.section-body .callout` already sets a floating margin at the same
specificity, and a bare `.refine-status` loses to it. A rule that looks like it
should work and does nothing is usually this.

`.callout` has `warn`, `error` and `ok` variants. `ok` keeps ordinary text
colour and puts the green in the tick alone: a whole line of green reads as a
warning at a glance.

## Wording rules

**Capitalisation.** Settled with Ben on 10/09/2026, and it is a rule rather
than a list so new strings have an answer:

| Kind | Case | Examples |
|---|---|---|
| Action (button, menu item) | Title case | `Refine`, `Stop and refine`, `Change…`, `Check again` |
| The NAME of a mode | Capitalised wherever it names the mode | `Refine` (overlay tag, dictation bar chip), `recording · Refine` |
| Generic status word | lower case | `recording`, `processing`, `refining`, `refined`, `trimmed` |

So the History tags are all lower case and agree with each other, and the mode
name keeps its capital everywhere it appears as a name. `hud.refining` reads
`Refining with {provider}… {secs}s` with a capital because it starts a
sentence, not because of the mode rule.

The two mode chips (`.bar-mode`, `.hud-mode-tag`) used to carry
`text-transform: uppercase` and shouted `REFINE` while every other status chip
was lower case. **That transform is gone and should stay gone**: with it, the
strings' own case is invisible and the rule above cannot be seen.

Two uppercase transforms DO remain and are deliberate, so do not "fix" them to
match the table: `.badge-lang` (the language tag, `EN`, which is an
abbreviation rather than a word) and `.settings-section h2` (section headings
such as `HOTKEYS`, which are typographic labels rather than prose). Both are
uppercase in CSS while their strings stay lower case, which is the right way
round: the string carries the meaning and the stylesheet carries the styling.

**Voice.** UK/Australian English. No em dashes or en dashes anywhere, in the
UI or in these docs. Hints explain the *why* when the why is not obvious, and
name the actual thing rather than talking in the abstract: "choose the claude
program file, or the .app bundle that holds it" beats "select the executable".

## i18n

`src/i18n/`, five locales: `en`, `fr`, `es`, `de`, `pt`. Deliberately
dependency-free, a synchronous `t()` over a few hundred fixed strings, because
the app ships offline and an i18n framework would add a loader and an async
init for no gain.

**English is the fallback**, so a key missing from a locale renders in English
rather than as a raw key or a blank. That makes parity desirable rather than
fatal, but check it anyway after adding keys:

```bash
node -e "
const fs=require('fs');
const files=['en','de','es','fr','pt'];
const keys=files.map(f=>new Set([...fs.readFileSync('src/i18n/'+f+'.ts','utf8').matchAll(/^\s+'([^']+)':/gm)].map(m=>m[1])));
files.forEach((f,i)=>console.log(f,'missing',[...keys[0]].filter(k=>!keys[i].has(k)),'extra',[...keys[i]].filter(k=>!keys[0].has(k))));
"
```

Two rules learned the hard way:

- **When the change is only the case, change only the case.** A bulk edit that
  "fixed" `hud.refineTag` also replaced French `reformuler` with `affiner` and
  German `Überarbeiten` with `verfeinern`, quietly changing established
  terminology. Reverted.
- **German nouns are already capitalised**, so a capitalisation pass over the
  other locales usually leaves `de` untouched. Check before editing it.
- Interpolation is `{name}` and is substituted by `t(key, { name })`. Used for
  the provider name, versions, and the tool name in the path guidance.

## Platform differences in the UI

`IS_MAC` in `SettingsView.tsx` is `navigator.userAgent.includes('Mac')`. It
chooses:

- key labels (`Fn / Globe` versus `Right Ctrl` / the Copilot key),
- the "prefer AX insert" toggle, which only exists on macOS,
- the path guidance, which names `.app` bundles on macOS and `.exe` / `.cmd` on
  Windows.

Everything else is shared. There is no per-platform stylesheet.

## Animation

The rules are commented at length in `DictationBar.tsx` around the morph, and
they are not obvious:

- **Transform and opacity only.** An earlier version animated `border-radius`
  and `background-color`; neither is compositable, so every frame repainted a
  full-width element carrying a large soft shadow and a 320ms expansion arrived
  in about three visible jumps. The accent fill is a `.bar-tint` overlay that
  fades instead.
- A Web Animations morph plus a CSS transition on the same properties gives the
  close two phases. `fill: 'forwards'` holds the last frame, `data-morph` kills
  the transitions while it runs, and the held fill must be CANCELLED before any
  early return or the bar can never open again.
- Final timings 520ms out, 360ms back, plain ease-out. A spring overshoot adds
  a second beat to something already reading as steppy.
- Reduce motion in Settings is the escape hatch and names the bar in its hint,
  so no separate animation setting is needed.
- The continuous cost while recording is the level meter re-rendering its bars
  (10/s, deferred until the morph finishes), NOT the morph.

**Animation timing cannot be judged in the Claude Code browser pane**: it does
not paint a background tab, so rAF never runs and timelines freeze. Assert end
states with `anim.finish()` and judge feel in the real app. See TESTING.md.

## A React contract that is easy to get wrong

`SettingsView`'s `set()` returns the save promise:

```ts
const set = (patch: (draft: Settings) => void): Promise<void> => { …; return onSave(next); };
```

It has to, because `save()` in `App.tsx` calls `setSettings(next)` **before**
`await api.setSettings(next)`. React re-renders with the new value while the
Rust side may still hold the old one. So:

> **Anything that reacts to a settings change by asking the Rust side must
> await the save first.**

The Refine probe was keyed on the provider through an effect and did not, so
switching the tool to Codex could report back on Claude. The probe is now
keyed on `refine.enabled` only, and the provider select awaits the save then
probes explicitly. The path picker and the "Back to automatic" button do the
same.
