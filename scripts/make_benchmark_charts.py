#!/usr/bin/env python3
"""Generate README benchmark charts (SVG). Data: measured 2026-10-12/13, Linux x86_64.

Regenerate: python3 scripts/make_benchmark_charts.py
"""
import html
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / 'docs'

W = 880
PAD_L = 40
BAR_X0 = 252.0          # bars start after the label column
BAR_X1 = W - 150.0      # leave room for value labels
ROW_H = 32
BAR_H = 12

AMBER = '#f59e0b'
ZINC = '#52525b'
ZINC_HI = '#71717a'
MARK = '#e4e4e7'
BG = '#17171c'
BORDER = '#2e2e33'
GRID = '#27272a'
TXT = '#f4f4f5'
SUB = '#a1a1aa'
NAME = '#d4d4d8'
MONO = "ui-monospace, 'SF Mono', 'Cascadia Code', Menlo, Consolas, monospace"
SANS = "-apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif"


def esc(s):
    return html.escape(str(s), quote=True)


class Svg:
    def __init__(self, height):
        self.h = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{height}" '
            f'viewBox="0 0 {W} {height}" font-family="{esc(SANS)}">',
            f'<rect x="0.5" y="0.5" width="{W-1}" height="{height-1}" rx="12" fill="{BG}" stroke="{BORDER}"/>',
        ]

    def text(self, x, y, s, size=13, fill=TXT, weight=400, family=SANS, anchor='start', spacing=None):
        extra = f' letter-spacing="{spacing}"' if spacing else ''
        self.parts.append(
            f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" fill="{fill}" font-weight="{weight}" '
            f'font-family="{esc(family)}" text-anchor="{anchor}"{extra}>{esc(s)}</text>')

    def rect(self, x, y, w, h, fill, rx=3):
        assert x >= 0 and y >= 0 and w >= 0 and x + w <= W, (x, y, w, h)
        self.parts.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="{rx}" fill="{fill}"/>')

    def vline(self, x, y1, y2, stroke=GRID):
        self.parts.append(f'<line x1="{x:.1f}" y1="{y1:.1f}" x2="{x:.1f}" y2="{y2:.1f}" stroke="{stroke}" stroke-width="1"/>')

    def save(self, path):
        self.parts.append('</svg>')
        path.write_text('\n'.join(self.parts), encoding='utf-8')


def log_scale(lo, hi, x0=BAR_X0, x1=BAR_X1):
    l0, l1 = math.log10(lo), math.log10(hi)
    return lambda v: x0 + (math.log10(max(v, lo)) - l0) / (l1 - l0) * (x1 - x0)


def lin_scale(lo, hi, x0=BAR_X0, x1=BAR_X1):
    return lambda v: x0 + (min(max(v, lo), hi) - lo) / (hi - lo) * (x1 - x0)


def panel(svg, y, title, rows, scale, winner, grid_ticks=None, notes=()):
    """rows: [(name, value, display, None)]. Returns y after the panel."""
    svg.text(PAD_L, y, title, 11.5, SUB, 600, spacing='1.1')
    top = y + 14
    bottom = top + len(rows) * ROW_H - 10
    if grid_ticks:
        for v, lbl in grid_ticks:
            x = scale(v)
            svg.vline(x, top, bottom)
            svg.text(x, bottom + 14, lbl, 10.5, ZINC_HI, family=MONO, anchor='middle')
    for i, (name, value, display, extra) in enumerate(rows):
        ry = top + i * ROW_H
        best = name == winner
        svg.text(PAD_L, ry + 11, name, 13, TXT if best else NAME, 600 if best else 400)
        bx = scale(value)
        svg.rect(BAR_X0, ry, max(2.0, bx - BAR_X0), BAR_H, AMBER if best else ZINC)
        vx = bx + 10
        assert vx <= W - 130, f'label overflow: {name} at {vx}'
        svg.text(vx, ry + 11, display, 12.5, TXT if best else NAME, 600 if best else 400, family=MONO)
        if extra:
            svg.text(vx + len(display) * 7.6 + 10, ry + 11, extra, 11, ZINC_HI, family=MONO)
    ycur = bottom
    if grid_ticks:
        ycur = bottom + 24
    else:
        ycur = bottom + 16
    for note in notes:
        ycur += 12
        svg.text(PAD_L, ycur, note, 11, ZINC_HI)
    return ycur + 16


def chart1():
    s = Svg(660)
    s.text(PAD_L, 46, 'Накладные расходы харнесса: Takiza против pi, openclaude, opencode', 19, TXT, 600)
    s.text(PAD_L, 68, 'Замер 2026-10-12 · Linux x86_64 · Node.js 22.23.1 · CLI из npm, Takiza — cargo build --release', 12.5, SUB)

    y = 100
    y = panel(
        s, y, 'Холодный старт --version, мс · медиана 30 запусков · лог. шкала',
        [('Takiza 0.1.7', 8.3, '8.3 мс', None),
         ('pi 1.1.0', 415.4, '415 мс', None),
         ('openclaude 0.31.0', 465.0, '465 мс', None),
         ('opencode 1.18.29', 565.6, '566 мс', None)],
        log_scale(5, 800), 'Takiza 0.1.7',
        grid_ticks=[(10, '10'), (100, '100'), (500, '500')])

    y = panel(
        s, y, 'Пик RSS процесса --version, МБ',
        [('Takiza 0.1.7', 11.7, '11.7 МБ', None),
         ('pi 1.1.0', 96.8, '97 МБ', None),
         ('openclaude 0.31.0', 150.4, '150 МБ', None),
         ('opencode 1.18.29', 186.1, '186 МБ', None)],
        lin_scale(0, 200), 'Takiza 0.1.7')

    y = panel(
        s, y, 'Размер дистрибутива, МБ',
        [('Takiza 0.1.7', 27.5, '27.5 МБ · один бинарник', None),
         ('openclaude 0.31.0', 43.0, '43 МБ', None),
         ('pi 1.1.0', 166.0, '166 МБ', None),
         ('opencode 1.18.29', 353.0, '353 МБ', None)],
        lin_scale(0, 370), 'Takiza 0.1.7',
        notes=['npm-пакет takiza с бинарниками всех платформ — 141 МБ; 27.5 МБ — сборка из исходников (cargo build --release).'])

    s.text(PAD_L, s.h - 14, 'Меньше — лучше. Методика и команды воспроизведения — в README.', 11, ZINC_HI)
    return s


def chart2():
    s = Svg(700)
    s.text(PAD_L, 46, 'Одинаковая модель: NVIDIA NIM GLM-5.3-Flash · «привет как дела?»', 19, TXT, 600)
    s.text(PAD_L, 68, 'Замеры 12–13.10.2026 · от запуска CLI до готового ответа · Linux x86_64, прокси до integrate.api.nvidia.com', 12.5, SUB)

    y = 100
    y = panel(
        s, y, 'Вечер, NIM свободен · медиана 3 прогонов, рядом — мин–макс',
        [('opencode 1.18.29', 3.2, '3.2 с', 'разброс 3.2–3.4'),
         ('pi 1.1.0', 3.9, '3.9 с', 'разброс 3.8–14.1'),
         ('openclaude 0.31.0', 5.9, '5.9 с', 'разброс 5.8–10.6'),
         ('Takiza 0.1.7', 6.8, '6.8 с', 'разброс 5.5–14.3')],
        lin_scale(0, 8.5), 'opencode 1.18.29',
        grid_ticks=[(0, '0'), (2, '2'), (4, '4'), (6, '6'), (8, '8')])

    y = panel(
        s, y, 'День, NIM перегружен · ручной замер, по одному прогону',
        [('opencode 1.18.29', 63, '63 с', None),
         ('pi 1.1.0', 50, '50 с', None),
         ('openclaude 0.31.0', 53, '53 с', None),
         ('Takiza 0.1.7', 46, '46 с', None)],
        lin_scale(0, 70), 'Takiza 0.1.7',
        grid_ticks=[(0, '0'), (20, '20'), (40, '40'), (60, '60')])

    y = panel(
        s, y, 'Контекст одного запроса, входные токены · стабилен в обеих сериях',
        [('pi 1.1.0', 1966, '≈2 000', None),
         ('Takiza 0.1.7', 7004, '7 004 *', None),
         ('opencode 1.18.29', 7505, '7 505', None),
         ('openclaude 0.31.0', 11169, '≈11 200', None)],
        lin_scale(0, 12000), 'pi 1.1.0',
        grid_ticks=[(0, '0'), (4000, '4K'), (8000, '8K'), (12000, '12K')],
        notes=['* у takiza провайдер отдаёт только сумму input+reasoning+output; разбивки нет.',
               'У openclaude 10 880 из 11 169 входных токенов на повторах обслуживает prompt-cache.'])

    s.text(PAD_L, s.h - 46, 'Базовая линия днём: голый запрос к NIM без харнесса — 35–57 с (25 промпт-токенов), с ~7.6K промптом — 34–35 с.', 11, ZINC_HI)
    s.text(PAD_L, s.h - 30, 'Рейтинг скорости меняется с нагрузкой NIM: вечером быстрее всех opencode, днём — takiza. Ручной замер takiza подкреплён', 11, ZINC_HI)
    s.text(PAD_L, s.h - 14, 'автоматическим контролем (медиана 46.1 с, k=3). opencode дополнительно тратит отдельный запрос на заголовок сессии.', 11, ZINC_HI)
    return s


def main():
    DOCS.mkdir(exist_ok=True)
    chart1().save(DOCS / 'bench-overhead.svg')
    chart2().save(DOCS / 'bench-nim-glm.svg')
    import xml.etree.ElementTree as ET
    for f in ['bench-overhead.svg', 'bench-nim-glm.svg']:
        ET.parse(DOCS / f)
        print(f'OK {DOCS / f}')


if __name__ == '__main__':
    main()
