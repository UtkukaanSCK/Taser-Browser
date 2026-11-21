"""Draws the README's animated SVGs into docs/media/.

    python docs/media/make_media.py

Each picture is written twice, `<name>-light.svg` and `<name>-dark.svg`, and the
README picks one with <picture> and prefers-color-scheme. Animations are plain
CSS inside the SVG (GitHub shows them in an <img>), honour
prefers-reduced-motion, and use no fonts, scripts or links from outside.
Deterministic: the same input gives the same bytes.
"""

import os

HERE = os.path.dirname(os.path.abspath(__file__))

# Same accent and neutrals as ui/input.css; ICON is assets/make_icon.py's BLUE.
ICON = "#2F62E9"
THEMES = {
    "light": {
        "bg0": "#ffffff", "bg1": "#f4f6fb", "ink": "#18181b", "muted": "#52525b",
        "faint": "#a1a1aa", "line": "#d4d4d8", "strip": "#f4f4f5", "surface": "#ffffff",
        "hover": "#e4e4e7", "accent": "#2f6fe0", "accent2": "#7aa7ff", "glow": "#2f6fe0",
        "glow_op": "0.10", "dot": "#d9dce5", "chip": "#eef2fb", "chip_ink": "#1e3a8a",
        "ok": "#15803d", "shadow": "0.10",
    },
    "dark": {
        "bg0": "#0d0f14", "bg1": "#141823", "ink": "#f4f4f5", "muted": "#a1a1aa",
        "faint": "#71717a", "line": "#3f3f46", "strip": "#1c1c1e", "surface": "#2c2c2e",
        "hover": "#38383b", "accent": "#5b8cff", "accent2": "#9db8ff", "glow": "#3b6cff",
        "glow_op": "0.22", "dot": "#232838", "chip": "#1b2340", "chip_ink": "#c7d6ff",
        "ok": "#4ade80", "shadow": "0.45",
    },
}

SANS = "'Segoe UI Variable Display','Segoe UI',system-ui,-apple-system,'Helvetica Neue',Arial,sans-serif"
MONO = "ui-monospace,'Cascadia Mono',Consolas,'SFMono-Regular',Menlo,monospace"

REDUCED = "@media (prefers-reduced-motion: reduce){*{animation:none!important}}"


def icon(x, y, s):
    """The app icon: blue rounded square with a round-capped white X."""
    r = s * 0.22
    a, b = x + s * 0.30, x + s * 0.70
    c, d = y + s * 0.30, y + s * 0.70
    w = s * 0.15
    return (
        f'<rect x="{x}" y="{y}" width="{s}" height="{s}" rx="{r:.1f}" fill="{ICON}"/>'
        f'<path d="M{a:.1f} {c:.1f}L{b:.1f} {d:.1f}M{b:.1f} {c:.1f}L{a:.1f} {d:.1f}" '
        f'stroke="#fff" stroke-width="{w:.1f}" stroke-linecap="round"/>'
    )


def banner(t):
    W, H = 1200, 420
    # Browser mock on the right.
    mx, my, mw, mh = 640, 62, 500, 300
    chips = ["Rust", "WebView2", "1.66 MB", "MIT"]
    chip_svg, cx = [], 64
    for i, label in enumerate(chips):
        w = 18 + len(label) * 9.6
        chip_svg.append(
            f'<g class="chip" style="animation-delay:{0.35 + i * 0.12:.2f}s">'
            f'<rect x="{cx}" y="318" width="{w:.0f}" height="32" rx="16" fill="{t["chip"]}" stroke="{t["line"]}"/>'
            f'<text x="{cx + w / 2:.1f}" y="339" text-anchor="middle" font-size="15" font-weight="600" fill="{t["chip_ink"]}">{label}</text></g>'
        )
        cx += w + 10

    tab = lambda x, cls, title, fav: (
        f'<g class="{cls}"><rect x="{x}" y="{my + 12}" width="128" height="30" rx="8" fill="{t["surface"]}" stroke="{t["line"]}"/>'
        f'<circle cx="{x + 18}" cy="{my + 27}" r="6" fill="{fav}"/>'
        f'<rect x="{x + 32}" y="{my + 23}" width="{title}" height="8" rx="4" fill="{t["faint"]}" opacity=".7"/>'
        f'<path d="M{x + 110} {my + 23}l8 8m0-8l-8 8" stroke="{t["faint"]}" stroke-width="1.6" stroke-linecap="round"/></g>'
    )
    url_x, url_y = mx + 84, my + 76
    url = "example.com/hello-x-browser"
    url_w = len(url) * 8.4

    skel = []
    for i, (w, h, gap) in enumerate([(300, 18, 0), (420, 10, 34), (380, 10, 54), (400, 10, 74), (240, 10, 94)]):
        skel.append(
            f'<rect class="sk" style="animation-delay:{i * 0.08:.2f}s" x="{mx + 32}" y="{my + 130 + gap}" '
            f'width="{w}" height="{h}" rx="{h / 2}" fill="{t["accent"] if i == 0 else t["faint"]}" opacity="{0.9 if i == 0 else 0.35}"/>'
        )
    tiles = "".join(
        f'<rect class="sk" style="animation-delay:{0.4 + i * 0.07:.2f}s" x="{mx + 32 + i * 112}" y="{my + 240}" width="96" height="40" rx="10" '
        f'fill="{t["hover"]}"/>' for i in range(4)
    )

    css = f"""
text{{font-family:{SANS}}}
.mono{{font-family:{MONO}}}
.rise{{animation:rise .9s cubic-bezier(.2,.8,.2,1) both}}
.chip{{animation:rise .7s cubic-bezier(.2,.8,.2,1) both}}
@keyframes rise{{from{{opacity:0;transform:translateY(14px)}}to{{opacity:1;transform:none}}}}
.glow{{animation:drift 14s ease-in-out infinite alternate;transform-origin:center}}
@keyframes drift{{from{{transform:translate(-30px,10px) scale(1)}}to{{transform:translate(40px,-20px) scale(1.15)}}}}
.mock{{animation:float 6s ease-in-out infinite}}
@keyframes float{{0%,100%{{transform:translateY(0)}}50%{{transform:translateY(-6px)}}}}
.t2{{animation:t2 9s infinite both}} .t3{{animation:t3 9s infinite both}}
@keyframes t2{{0%,6%{{opacity:0;transform:translateX(-24px)}}12%,94%{{opacity:1;transform:none}}100%{{opacity:0}}}}
@keyframes t3{{0%,14%{{opacity:0;transform:translateX(-24px)}}20%,94%{{opacity:1;transform:none}}100%{{opacity:0}}}}
.type{{transform-box:fill-box;transform-origin:left center;animation:type 9s steps({len(url)}) infinite both}}
@keyframes type{{0%,24%{{transform:scaleX(0)}}46%,94%{{transform:scaleX(1)}}100%{{transform:scaleX(0)}}}}
.caret{{animation:caret 9s linear infinite}}
@keyframes caret{{0%,24%{{transform:translateX(0)}}46%,100%{{transform:translateX({url_w:.0f}px)}}}}
.blink{{animation:blink 1s steps(1) infinite}}
@keyframes blink{{50%{{opacity:0}}}}
.lock{{animation:lock 9s infinite both}}
@keyframes lock{{0%,47%{{opacity:0;transform:scale(.6)}}52%,94%{{opacity:1;transform:none}}100%{{opacity:0}}}}
.lock{{transform-box:fill-box;transform-origin:center}}
.bar{{transform-box:fill-box;transform-origin:left center;animation:bar 9s cubic-bezier(.4,0,.2,1) infinite both}}
@keyframes bar{{0%,47%{{transform:scaleX(0);opacity:1}}62%{{transform:scaleX(1);opacity:1}}66%,100%{{transform:scaleX(1);opacity:0}}}}
.sk{{animation:sk 9s infinite both}}
@keyframes sk{{0%,60%{{opacity:0;transform:translateY(8px)}}68%,94%{{opacity:1;transform:none}}100%{{opacity:0}}}}
{REDUCED}
@media (prefers-reduced-motion: reduce){{.caret{{display:none}}}}
"""
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="X-Browser: a light, minimal desktop browser in Rust on WebView2">
<style>{css}</style>
<defs>
<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{t["bg0"]}"/><stop offset="1" stop-color="{t["bg1"]}"/></linearGradient>
<radialGradient id="glow"><stop offset="0" stop-color="{t["glow"]}" stop-opacity="{t["glow_op"]}"/><stop offset="1" stop-color="{t["glow"]}" stop-opacity="0"/></radialGradient>
<pattern id="dots" width="22" height="22" patternUnits="userSpaceOnUse"><circle cx="2" cy="2" r="1.3" fill="{t["dot"]}"/></pattern>
<linearGradient id="title" x1="0" x2="1"><stop offset="0" stop-color="{t["ink"]}"/><stop offset="1" stop-color="{t["accent"]}"/></linearGradient>
<linearGradient id="barg" x1="0" x2="1"><stop offset="0" stop-color="{t["accent"]}"/><stop offset="1" stop-color="{t["accent2"]}"/></linearGradient>
<filter id="sh" x="-20%" y="-20%" width="140%" height="150%"><feDropShadow dx="0" dy="18" stdDeviation="20" flood-color="#000" flood-opacity="{t["shadow"]}"/></filter>
<clipPath id="urlclip"><rect class="type" x="{url_x}" y="{url_y - 16}" height="24" width="{url_w:.0f}"/></clipPath>
<clipPath id="mockclip"><rect x="{mx}" y="{my}" width="{mw}" height="{mh}" rx="14"/></clipPath>
</defs>
<rect width="{W}" height="{H}" rx="24" fill="url(#bg)"/>
<rect width="{W}" height="{H}" rx="24" fill="url(#dots)" opacity=".8"/>
<g class="glow"><circle cx="880" cy="200" r="320" fill="url(#glow)"/></g>
<g class="rise">{icon(64, 70, 84)}</g>
<g class="rise" style="animation-delay:.12s"><text x="62" y="222" font-size="72" font-weight="700" letter-spacing="-2" fill="url(#title)">X-Browser</text></g>
<g class="rise" style="animation-delay:.24s"><text x="64" y="262" font-size="22" fill="{t["muted"]}">A light, minimal desktop browser in Rust</text>
<text x="64" y="292" font-size="22" fill="{t["muted"]}">on the system WebView, built with wry + tao only.</text></g>
{"".join(chip_svg)}
<g class="mock"><g filter="url(#sh)">
<rect x="{mx}" y="{my}" width="{mw}" height="{mh}" rx="14" fill="{t["surface"]}" stroke="{t["line"]}"/>
</g>
<g clip-path="url(#mockclip)">
<rect x="{mx}" y="{my}" width="{mw}" height="98" fill="{t["strip"]}"/>
<line x1="{mx}" x2="{mx + mw}" y1="{my + 98}" y2="{my + 98}" stroke="{t["line"]}"/>
{tab(mx + 12, "t1", 60, ICON)}
{tab(mx + 146, "t2", 70, "#f59e0b")}
{tab(mx + 280, "t3", 50, "#10b981")}
<path d="M{mx + 424} {my + 27}h12M{mx + 430} {my + 21}v12" stroke="{t["muted"]}" stroke-width="1.8" stroke-linecap="round"/>
<path d="M{mx + 22} {url_y - 4}l-6 0m0 0l4-4m-4 4l4 4" stroke="{t["muted"]}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
<path d="M{mx + 40} {url_y - 4}l6 0m0 0l-4-4m4 4l-4 4" stroke="{t["faint"]}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
<rect x="{mx + 54}" y="{url_y - 20}" width="{mw - 100}" height="32" rx="16" fill="{t["surface"]}" stroke="{t["line"]}"/>
<g class="lock"><rect x="{mx + 64}" y="{url_y - 7}" width="11" height="9" rx="2" fill="{t["ok"]}"/><path d="M{mx + 66.5} {url_y - 7}v-3a3 3 0 0 1 6 0v3" stroke="{t["ok"]}" stroke-width="1.8" fill="none"/></g>
<text x="{url_x}" y="{url_y}" font-size="14" class="mono" fill="{t["ink"]}" clip-path="url(#urlclip)">{url}</text>
<g class="caret"><rect class="blink" x="{url_x + 1}" y="{url_y - 13}" width="1.6" height="17" fill="{t["accent"]}"/></g>
<circle cx="{mx + mw - 26}" cy="{url_y - 4}" r="2" fill="{t["muted"]}"/><circle cx="{mx + mw - 26}" cy="{url_y - 10}" r="2" fill="{t["muted"]}"/><circle cx="{mx + mw - 26}" cy="{url_y + 2}" r="2" fill="{t["muted"]}"/>
<rect class="bar" x="{mx}" y="{my + 96}" height="3" width="{mw}" fill="url(#barg)"/>
{"".join(skel)}
{tiles}
</g></g>
</svg>
"""


def timing(t):
    """ui-ready and binary size per release, from README "Startup timing"."""
    rows = [  # label, ui-ready ms, binary bytes
        ("v0.1", 308, 684_544),
        ("v0.2", 362, 1_260_032),
        ("v0.3", 421, 1_656_832),
    ]
    W, H = 1200, 300
    x0, bw_max, ms_max = 150, 380, 500
    s0 = 700
    mb_max = 2.0
    out = []
    for i, (lab, ms, b) in enumerate(rows):
        y = 104 + i * 58
        w = ms / ms_max * bw_max
        mb = b / 1_000_000
        w2 = mb / mb_max * bw_max
        d = 0.15 + i * 0.15
        out.append(
            f'<text x="{x0 - 20}" y="{y + 21}" text-anchor="end" font-size="17" font-weight="600" fill="{t["ink"]}">{lab}</text>'
            f'<rect x="{x0}" y="{y}" width="{bw_max}" height="30" rx="8" fill="{t["hover"]}" opacity=".55"/>'
            f'<rect class="grow" style="animation-delay:{d:.2f}s" x="{x0}" y="{y}" width="{w:.1f}" height="30" rx="8" fill="url(#ga)"/>'
            f'<text class="fade" style="animation-delay:{d + 0.6:.2f}s" x="{x0 + w + 10:.1f}" y="{y + 21}" font-size="16" font-weight="600" fill="{t["ink"]}">{ms} ms</text>'
            f'<rect x="{s0}" y="{y}" width="{bw_max}" height="30" rx="8" fill="{t["hover"]}" opacity=".55"/>'
            f'<rect class="grow" style="animation-delay:{d + 0.2:.2f}s" x="{s0}" y="{y}" width="{w2:.1f}" height="30" rx="8" fill="url(#gb)"/>'
            f'<text class="fade" style="animation-delay:{d + 0.8:.2f}s" x="{s0 + w2 + 10:.1f}" y="{y + 21}" font-size="16" font-weight="600" fill="{t["ink"]}">{mb:.2f} MB</text>'
        )
    css = f"""
text{{font-family:{SANS}}}
.grow{{transform-box:fill-box;transform-origin:left center;animation:grow 1.1s cubic-bezier(.2,.8,.2,1) both}}
@keyframes grow{{from{{transform:scaleX(0)}}to{{transform:scaleX(1)}}}}
.fade{{animation:fade .5s ease-out both}}
@keyframes fade{{from{{opacity:0}}to{{opacity:1}}}}
{REDUCED}
"""
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="Start-up to ui-ready: v0.1 308 ms, v0.2 362 ms, v0.3 421 ms. Binary: 0.68, 1.26 and 1.66 MB.">
<style>{css}</style>
<defs>
<linearGradient id="ga" x1="0" x2="1"><stop offset="0" stop-color="{t["accent"]}"/><stop offset="1" stop-color="{t["accent2"]}"/></linearGradient>
<linearGradient id="gb" x1="0" x2="1"><stop offset="0" stop-color="#7c5cff"/><stop offset="1" stop-color="#b9a6ff"/></linearGradient>
</defs>
<rect width="{W}" height="{H}" rx="20" fill="{t["bg1"]}" stroke="{t["line"]}"/>
<text x="{x0}" y="56" font-size="20" font-weight="700" fill="{t["ink"]}">Start-up to a usable window</text>
<text x="{x0}" y="80" font-size="14" fill="{t["muted"]}">ui-ready, ms since the OS created the process (median of 10)</text>
<text x="{s0}" y="56" font-size="20" font-weight="700" fill="{t["ink"]}">Release binary</text>
<text x="{s0}" y="80" font-size="14" fill="{t["muted"]}">size of target/release/x-browser.exe</text>
{"".join(out)}
<text x="{x0}" y="{H - 18}" font-size="13" fill="{t["faint"]}">Release builds, scratch profile, one blank tab. v0.1 and v0.2 measured 2026-11-17, v0.3 2026-11-19. Method and every row: README "Startup timing".</text>
</svg>
"""


def architecture(t):
    W, H = 1200, 470
    css = f"""
text{{font-family:{SANS}}}
.mono{{font-family:{MONO}}}
.flow{{stroke-dasharray:6 8;animation:flow 1.2s linear infinite}}
.flow.r{{animation-direction:reverse}}
@keyframes flow{{to{{stroke-dashoffset:-28}}}}
.pulse{{animation:pulse 2.4s ease-in-out infinite}}
@keyframes pulse{{0%,100%{{opacity:.35}}50%{{opacity:1}}}}
{REDUCED}
"""

    def box(x, y, w, h, title, sub, accent=False, dash=False):
        stroke = t["accent"] if accent else t["line"]
        extra = ' stroke-dasharray="6 6"' if dash else ""
        return (
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14" fill="{t["surface"]}" stroke="{stroke}" stroke-width="1.5"{extra}/>'
            f'<text x="{x + 18}" y="{y + 30}" font-size="17" font-weight="700" fill="{t["ink"]}">{title}</text>'
            + "".join(
                f'<text x="{x + 18}" y="{y + 54 + i * 21}" font-size="14" fill="{t["muted"]}">{line}</text>'
                for i, line in enumerate(sub)
            )
        )

    win_x, win_y, win_w, win_h = 40, 40, 640, 390
    core_x = 840
    parts = [
        f'<rect x="{win_x}" y="{win_y}" width="{win_w}" height="{win_h}" rx="18" fill="{t["bg1"]}" stroke="{t["line"]}" stroke-width="1.5"/>',
        f'<text x="{win_x + 20}" y="{win_y + 30}" font-size="13" font-weight="700" letter-spacing="1.5" fill="{t["faint"]}">ONE TAO WINDOW · THREE KINDS OF WEBVIEW2</text>',
        box(win_x + 20, win_y + 48, 600, 84, "Tab strip and address bar", ["ui/index.html · vanilla JS + Tailwind · profile XbUi, no extensions"]),
        box(win_x + 20, win_y + 150, 600, 220, "Content tabs", ["one WebView2 per tab · content profile", "Chrome extensions (MV3), uBlock Origin Lite", "tracking prevention, HTTPS-only, GPC"]),
        box(win_x + 300, win_y + 236, 300, 118, "Overlay", ["menus, suggestions, site panel,", "questions, Uzantılar · profile XbUi"], dash=True),
        box(core_x, 40, 320, 390, "Rust core", []),
    ]
    mods = [
        ("app.rs", "state, layout, IPC"),
        ("tabs.rs · session.rs", "tabs, restore"),
        ("policy.rs · privacy.rs", "indicator, privacy switches"),
        ("history.rs · bookmarks.rs", "local JSON files"),
        ("webstore.rs · crx.rs", "signed installs"),
        ("native/*", "WebView2 COM hooks"),
    ]
    for i, (m, d) in enumerate(mods):
        y = 96 + i * 54
        parts.append(
            f'<rect x="{core_x + 18}" y="{y}" width="284" height="44" rx="10" fill="{t["hover"]}" opacity=".6"/>'
            f'<text x="{core_x + 32}" y="{y + 20}" font-size="14" font-weight="600" class="mono" fill="{t["ink"]}">{m}</text>'
            f'<text x="{core_x + 32}" y="{y + 37}" font-size="13" fill="{t["muted"]}">{d}</text>'
        )
    # Flows: commands in, state out.
    for y, label in [(130, "IPC commands"), (260, "navigation, policy"), (340, "draw / close")]:
        parts.append(
            f'<path class="flow" d="M{win_x + win_w + 4} {y - 6}H{core_x - 8}" stroke="{t["accent"]}" stroke-width="2.2" fill="none"/>'
            f'<path class="flow r" d="M{win_x + win_w + 4} {y + 6}H{core_x - 8}" stroke="{t["faint"]}" stroke-width="2.2" fill="none"/>'
            f'<text x="{(win_x + win_w + core_x) / 2:.0f}" y="{y - 14}" text-anchor="middle" font-size="12" fill="{t["muted"]}">{label}</text>'
        )
    parts.append(f'<circle class="pulse" cx="{core_x + 296}" cy="64" r="5" fill="{t["ok"]}"/>')
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="Architecture: one window with a UI strip webview, one webview per content tab and an overlay webview, all driven by the Rust core over IPC.">
<style>{css}</style>
<rect width="{W}" height="{H}" rx="20" fill="{t["bg0"]}" stroke="{t["line"]}"/>
{"".join(parts)}
</svg>
"""


def main():
    for name, fn in (("banner", banner), ("timing", timing), ("architecture", architecture)):
        for theme, pal in THEMES.items():
            path = os.path.join(HERE, f"{name}-{theme}.svg")
            with open(path, "w", encoding="utf-8", newline="\n") as f:
                f.write(fn(pal))
            print(path)


if __name__ == "__main__":
    main()
