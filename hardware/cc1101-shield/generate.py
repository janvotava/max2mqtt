"""CC1101 shield v2 for D1-mini-sized boards: writes the KiCad project from this script.

Run with KiCad's own Python (it has the `pcbnew` module), from this directory:

    /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 generate.py

or `./build.sh`, which also runs KiCad's checks and exports everything for ordering. The board keeps v1's shape and every
hole position, so it fits the same boards and modules. What changed is in README.md.

Coordinates are in mm. Top left of the board is (100, 100); y grows downwards, as in KiCad.
"""

import json
import os
import uuid

import pcbnew

HERE = os.path.dirname(os.path.abspath(__file__))
NAME = "cc1101-shield"
FOOTPRINTS = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints"
SYMBOLS = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"


def uid(name):
    """Stable UUIDs, so regenerating gives the same files."""
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"max2mqtt/{NAME}/{name}"))


# ----- what is connected to what ---------------------------------------------------

# The D1 mini's headers, top to bottom, as seen from the top of the shield.
LEFT = ["RST", "A0", "D0", "D5", "D6", "D7", "D8", "3V3"]
RIGHT = ["TX", "RX", "D1", "D2", "D3", "D4", "GND", "5V"]
# The CC1101 module's 2 mm header, pin 1 to 8.
MODULE = ["VCC", "GND", "MOSI", "SCLK", "MISO", "GDO2", "GDO0", "CSN"]

# Net of each pin; None: not connected. GDO0 stays free: the firmware does not use it, and
# on v1 it was wired to D3, which is GPIO0 on the D1 mini and must be high at boot, while the
# CC1101 drives a clock on it after reset.
NETS = {
    "J1": [None, None, None, "SCK", "MISO", "MOSI", "CS", "+3V3"],
    "J2": [None, None, None, "GDO2", None, None, "GND", None],
    "U1": ["+3V3", "GND", "MOSI", "SCK", "MISO", "GDO2", None, "CS"],
}

# ----- geometry (from v1's Gerbers) --------------------------------------------------

LEFT_X, RIGHT_X = 101.349, 124.209
HEADER_Y = [102.080 + 2.54 * k for k in range(8)]
MODULE_X = [106.579 + 2.0 * k for k in range(8)]
MODULE_Y = 123.495
BOARD_RIGHT, BOARD_BOTTOM = 125.781, 128.829
# The notch at the bottom left, as on v1.
NOTCH_X, NOTCH_Y = 102.032, 121.844

SIGNAL, POWER = 0.3, 0.6


def mm(x, y):
    return pcbnew.VECTOR2I_MM(x, y)


# ----- schematic -------------------------------------------------------------------------


def conn_symbol():
    """Connector_Generic:Conn_01x08 from KiCad's library, to embed in the schematic."""
    text = open(os.path.join(SYMBOLS, "Connector_Generic.kicad_sym")).read()
    start = text.index('(symbol "Conn_01x08"')
    depth = 0
    for end in range(start, len(text)):
        depth += {"(": 1, ")": -1}.get(text[end], 0)
        if depth == 0:
            break
    return text[start : end + 1].replace('(symbol "Conn_01x08"', '(symbol "Connector_Generic:Conn_01x08"', 1)


def schematic():
    root = uid("schematic")
    parts = [
        ("J1", "D1 mini left", "max2mqtt:PinHeader_1x08_P2.54mm_Vertical", 60.96, LEFT),
        ("J2", "D1 mini right", "max2mqtt:PinHeader_1x08_P2.54mm_Vertical", 111.76, RIGHT),
        ("U1", "CC1101 868 MHz", "max2mqtt:CC1101_Module_1x08_P2.00mm", 162.56, MODULE),
    ]
    y0 = 76.2
    out = [
        "(kicad_sch",
        "\t(version 20250114)",
        '\t(generator "max2mqtt")',
        '\t(generator_version "9.0")',
        f'\t(uuid "{root}")',
        '\t(paper "A5")',
        "\t(title_block",
        '\t\t(title "CC1101 shield")',
        '\t\t(rev "2")',
        '\t\t(company "max2mqtt")',
        '\t\t(comment 1 "eQ-3 MAX! radio for D1-mini-sized ESP32 boards")',
        "\t)",
        "\t(lib_symbols",
        conn_symbol(),
        "\t)",
    ]

    def prop(name, value, x, y, hide=False, justify=None):
        effects = "(font (size 1.27 1.27))" + (f" (justify {justify})" if justify else "") + (" (hide yes)" if hide else "")
        return f'\t\t(property "{name}" "{value}" (at {x:.2f} {y:.2f} 0) (effects {effects}))'

    for ref, value, footprint, x, names in parts:
        sid = uid(f"symbol/{ref}")
        out += [
            "\t(symbol",
            '\t\t(lib_id "Connector_Generic:Conn_01x08")',
            f"\t\t(at {x:.2f} {y0:.2f} 0)",
            "\t\t(unit 1) (exclude_from_sim no) (in_bom yes) (on_board yes) (dnp no)",
            f'\t\t(uuid "{sid}")',
            prop("Reference", ref, x, y0 - 10.16),
            prop("Value", value, x, y0 + 12.7),
            prop("Footprint", footprint, x, y0, hide=True),
            prop("Datasheet", "~", x, y0, hide=True),
        ]
        out += [f'\t\t(pin "{k + 1}" (uuid "{uid(f"pin/{ref}/{k + 1}")}"))' for k in range(8)]
        out += [
            f'\t\t(instances (project "{NAME}" (path "/{root}" (reference "{ref}") (unit 1))))',
            "\t)",
        ]
        for k, net in enumerate(NETS[ref]):
            # Pin k ends at (x - 5.08, y0 - 7.62 + 2.54 k) on the sheet.
            px, py = x - 5.08, y0 - 7.62 + 2.54 * k
            if net is None:
                out.append(f'\t(no_connect (at {px:.2f} {py:.2f}) (uuid "{uid(f"nc/{ref}/{k}")}"))')
            else:
                out.append(
                    f'\t(label "{net}" (at {px:.2f} {py:.2f} 180) (effects (font (size 1.27 1.27)) '
                    f'(justify right bottom)) (uuid "{uid(f"label/{ref}/{k}")}"))'
                )
            # Pin names next to the pins, since the symbol hides them.
            out.append(
                f'\t(text "{names[k]}" (exclude_from_sim no) (at {x + 1.27:.2f} {py:.2f} 0) '
                f'(effects (font (size 1.27 1.27)) (justify left)) (uuid "{uid(f"text/{ref}/{k}")}"))'
            )
    out += ['\t(sheet_instances (path "/" (page "1")))', "\t(embedded_fonts no)", ")"]
    path = os.path.join(HERE, f"{NAME}.kicad_sch")
    open(path, "w").write("\n".join(out) + "\n")
    return {ref: uid(f"symbol/{ref}") for ref, *_ in parts}, root


# ----- board ---------------------------------------------------------------------------


LOCAL_LIB = os.path.join(HERE, "max2mqtt.pretty")


def footprint_library():
    """The two footprints this board uses, in a library next to it: KiCad's pin headers
    without their silkscreen (the board has its own labels), and for the module bigger pads.
    v1's 1.2 mm pads around 0.81 mm holes left a 0.19 mm ring and were hard to solder."""
    os.makedirs(LOCAL_LIB, exist_ok=True)
    for lib, name, new in [
        ("Connector_PinHeader_2.54mm", "PinHeader_1x08_P2.54mm_Vertical", "PinHeader_1x08_P2.54mm_Vertical"),
        ("Connector_PinHeader_2.00mm", "PinHeader_1x08_P2.00mm_Vertical", "CC1101_Module_1x08_P2.00mm"),
    ]:
        fp = pcbnew.FootprintLoad(os.path.join(FOOTPRINTS, lib + ".pretty"), name)
        fp.SetFPID(pcbnew.LIB_ID("max2mqtt", new))
        for item in fp.GraphicalItems():
            if item.GetLayer() == pcbnew.F_SilkS:
                item.SetLayer(pcbnew.F_Fab)
        if new.startswith("CC1101"):
            fp.SetLibDescription("CC1101 868 MHz module, 1x08 2.00 mm, 1.6 mm pads for hand soldering")
            for k in range(8):
                pad = fp.FindPadByNumber(str(k + 1))
                pad.SetSize(mm(1.6, 1.6))
                pad.SetDrillSize(mm(0.9, 0.9))
        pcbnew.PCB_IO_MGR.FindPlugin(pcbnew.PCB_IO_MGR.KICAD_SEXP).FootprintSave(LOCAL_LIB, fp)
    open(os.path.join(HERE, "fp-lib-table"), "w").write(
        '(fp_lib_table\n  (version 7)\n  (lib (name "max2mqtt") (type "KiCad") (uri "${KIPRJMOD}/max2mqtt.pretty") (options "") (descr "Footprints of this board"))\n)\n'
    )


def board(symbols):
    path = os.path.join(HERE, f"{NAME}.kicad_pcb")
    b = pcbnew.NewBoard(path)
    ds = b.GetDesignSettings()
    ds.SetCopperLayerCount(2)
    ds.m_MinClearance = pcbnew.FromMM(0.2)
    ds.m_TrackMinWidth = pcbnew.FromMM(0.2)
    ds.m_CopperEdgeClearance = pcbnew.FromMM(0.3)

    # Named as the schematic names them: local labels get the sheet path, free pins a name of
    # their own.
    nets = {}
    for name in sorted({n for pins in NETS.values() for n in pins if n}):
        nets[name] = pcbnew.NETINFO_ITEM(b, "/" + name)
        b.Add(nets[name])
    for ref, pins in NETS.items():
        for k, net in enumerate(pins):
            if net is None:
                free = f"unconnected-({ref}-Pin_{k + 1}-Pad{k + 1})"
                nets[free] = pcbnew.NETINFO_ITEM(b, free)
                b.Add(nets[free])

    def footprint(lib, name, ref, value, x, y, angle):
        fp = pcbnew.FootprintLoad(LOCAL_LIB, name)
        fp.SetReference(ref)
        fp.SetValue(value)
        fp.SetFPID(pcbnew.LIB_ID("max2mqtt", name))
        fp.SetPath(pcbnew.KIID_PATH(f"/{symbols[ref]}"))
        fp.SetPosition(mm(x, y))
        fp.SetOrientationDegrees(angle)
        fp.Reference().SetVisible(False)
        fp.Value().SetVisible(False)
        b.Add(fp)
        for k, net in enumerate(NETS[ref]):
            fp.FindPadByNumber(str(k + 1)).SetNet(nets[net or f"unconnected-({ref}-Pin_{k + 1}-Pad{k + 1})"])
        return fp

    footprint("Connector_PinHeader_2.54mm", "PinHeader_1x08_P2.54mm_Vertical", "J1", "D1 mini left", LEFT_X, HEADER_Y[0], 0)
    footprint("Connector_PinHeader_2.54mm", "PinHeader_1x08_P2.54mm_Vertical", "J2", "D1 mini right", RIGHT_X, HEADER_Y[0], 0)
    module = footprint(
        "Connector_PinHeader_2.00mm", "CC1101_Module_1x08_P2.00mm", "U1", "CC1101 868 MHz", MODULE_X[0], MODULE_Y, 90
    )
    for k in range(8):
        p = module.FindPadByNumber(str(k + 1)).GetPosition()
        assert abs(pcbnew.ToMM(p.x) - MODULE_X[k]) < 0.01 and abs(pcbnew.ToMM(p.y) - MODULE_Y) < 0.01, (k, p)

    def track(net, layer, width, *points):
        for (x1, y1), (x2, y2) in zip(points, points[1:]):
            t = pcbnew.PCB_TRACK(b)
            t.SetStart(mm(x1, y1))
            t.SetEnd(mm(x2, y2))
            t.SetWidth(pcbnew.FromMM(width))
            t.SetLayer(layer)
            t.SetNet(nets[net])
            b.Add(t)

    top, bottom = pcbnew.F_Cu, pcbnew.B_Cu
    hy, mx, my = HEADER_Y, MODULE_X, MODULE_Y
    # Tracks leave the header diagonally and run halfway between two pin rows, so they never
    # cross the pin labels printed next to the pins.
    half = 1.27

    def from_left(row, x_bend, pin):
        return [(LEFT_X, hy[row]), (LEFT_X + half, hy[row] + half), (x_bend, hy[row] + half),
                (mx[pin], hy[row] + half + (mx[pin] - x_bend)), (mx[pin], my)]

    # Power as real tracks: v1 had them only as copper fills, which pulled the heat away from
    # the iron and made those joints the hard ones.
    track("+3V3", top, POWER, (LEFT_X, hy[7]), (LEFT_X + half, hy[7] + half), (104.2, hy[7] + half),
          (mx[0], hy[7] + half + mx[0] - 104.2), (mx[0], my))
    gnd_y = 125.6
    track("GND", top, POWER, (mx[1], my), (mx[1], gnd_y), (122.3, gnd_y), (122.3, hy[6] + RIGHT_X - 122.3), (RIGHT_X, hy[6]))
    track("SCK", top, SIGNAL, *from_left(3, 110.6, 3))
    track("MOSI", top, SIGNAL, *from_left(5, 108.6, 2))
    track("GDO2", top, SIGNAL, (RIGHT_X, hy[3]), (RIGHT_X - half, hy[3] + half), (118.6, hy[3] + half),
          (mx[5], hy[3] + half + 118.6 - mx[5]), (mx[5], my))
    # On the bottom, where no labels are next to the header.
    track("MISO", bottom, SIGNAL, (LEFT_X, hy[4]), (mx[4] - 1.98, hy[4]), (mx[4], hy[4] + 1.98), (mx[4], my))
    # CS round under the module row along the bottom edge, into pin 8 from the side: the
    # space under the row holds the module's pin names on the bottom silkscreen.
    cs_y = 127.9
    track("CS", bottom, SIGNAL, (LEFT_X, hy[6]), (103.4, hy[6]), (103.4, cs_y - 1.0), (104.4, cs_y), (121.9, cs_y),
          (121.9, my + 1.321), (mx[7], my))

    # Outline: v1's shape, with rounded corners.
    def edge(shape):
        shape.SetLayer(pcbnew.Edge_Cuts)
        shape.SetWidth(pcbnew.FromMM(0.1))
        b.Add(shape)

    r = 1.0
    corners = [(100, 100), (BOARD_RIGHT, 100), (BOARD_RIGHT, BOARD_BOTTOM), (NOTCH_X, BOARD_BOTTOM), (NOTCH_X, NOTCH_Y), (100, NOTCH_Y)]
    rounded = {0: r, 1: r, 2: r, 3: 0.5, 4: 0, 5: 0.5}
    pts = []
    n = len(corners)
    for k, (x, y) in enumerate(corners):
        rk = rounded[k]
        px, py = corners[k - 1]
        nx, ny = corners[(k + 1) % n]

        def toward(ax, ay, d):
            length = ((ax - x) ** 2 + (ay - y) ** 2) ** 0.5
            return (x + (ax - x) * d / length, y + (ay - y) * d / length)

        if rk:
            a, c = toward(px, py, rk), toward(nx, ny, rk)
            # The arc's centre is r in from both edges; its midpoint lies on the way there, r from
            # the centre: (1 - 1/sqrt 2) of the corner-to-centre distance from the corner.
            cx, cy = a[0] + c[0] - x, a[1] + c[1] - y
            f = 1 - 2**-0.5
            mid = (x + (cx - x) * f, y + (cy - y) * f)
            pts.append(("arc", a, mid, c))
        else:
            pts.append(("pt", (x, y)))
    ends = [(p[1], p[3]) if p[0] == "arc" else (p[1], p[1]) for p in pts]
    for k, p in enumerate(pts):
        if p[0] == "arc":
            s = pcbnew.PCB_SHAPE(b, pcbnew.SHAPE_T_ARC)
            s.SetArcGeometry(mm(*p[1]), mm(*p[2]), mm(*p[3]))
            edge(s)
        line = pcbnew.PCB_SHAPE(b, pcbnew.SHAPE_T_SEGMENT)
        line.SetStart(mm(*ends[k][1]))
        line.SetEnd(mm(*ends[(k + 1) % n][0]))
        edge(line)

    # Ground on both sides, tied together with vias.
    for layer in (top, bottom):
        z = pcbnew.ZONE(b)
        z.SetLayer(layer)
        z.SetNet(nets["GND"])
        z.SetLocalClearance(pcbnew.FromMM(0.3))
        z.SetMinThickness(pcbnew.FromMM(0.25))
        z.SetPadConnection(pcbnew.ZONE_CONNECTION_THERMAL)
        z.SetThermalReliefGap(pcbnew.FromMM(0.3))
        z.SetThermalReliefSpokeWidth(pcbnew.FromMM(0.4))
        outline = z.Outline()
        outline.NewOutline()
        # The board's shape, 0.3 mm in from the edge.
        for p in pts:
            for q in ([p[1], p[2], p[3]] if p[0] == "arc" else [p[1]]):
                outline.Append(mm(*q))
        outline.Inflate(pcbnew.FromMM(-0.3), pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, pcbnew.FromMM(0.01))
        b.Add(z)
    for x, y in [(107.5, 105.9), (113.0, 105.9), (118.5, 105.9), (106.0, 108.4), (116.5, 106.4), (120.5, 113.5),
                 (106.5, 118.6), (119.0, 120.0), (124.4, 127.4)]:
        v = pcbnew.PCB_VIA(b)
        v.SetPosition(mm(x, y))
        v.SetWidth(pcbnew.FromMM(0.6))
        v.SetDrill(pcbnew.FromMM(0.3))
        v.SetNet(nets["GND"])
        b.Add(v)

    # Silkscreen.
    def text(s, x, y, size=0.8, layer=pcbnew.F_SilkS, angle=0, justify=pcbnew.GR_TEXT_H_ALIGN_CENTER):
        t = pcbnew.PCB_TEXT(b)
        t.SetText(s)
        t.SetPosition(mm(x, y))
        t.SetLayer(layer)
        t.SetTextSize(mm(size, size))
        t.SetTextThickness(pcbnew.FromMM(size * 0.15))
        t.SetTextAngleDegrees(angle)
        t.SetHorizJustify(justify)
        if layer == pcbnew.B_SilkS:
            t.SetMirrored(True)
        b.Add(t)

    left_labels = ["RST", "A0", "D0", "D5 SCK", "D6 MISO", "D7 MOSI", "D8 CS", "3V3"]
    right_labels = ["TX", "RX", "D1", "GDO2 D2", "D3", "D4", "GND", "5V"]
    for k in range(8):
        text(left_labels[k], LEFT_X + 1.5, hy[k], justify=pcbnew.GR_TEXT_H_ALIGN_LEFT)
        text(right_labels[k], RIGHT_X - 1.5, hy[k], justify=pcbnew.GR_TEXT_H_ALIGN_RIGHT)
    # The module's pin names under its pins, on the side where they are soldered.
    for k, name in enumerate(["3V3", "GND", "MOSI", "SCK", "MISO", "GDO2", "NC", "CS"]):
        text(name, mx[k], my + 1.3, size=0.8, layer=pcbnew.B_SilkS, angle=90, justify=pcbnew.GR_TEXT_H_ALIGN_LEFT)
    box = pcbnew.PCB_SHAPE(b, pcbnew.SHAPE_T_RECT)
    box.SetStart(mm(mx[0] - 1.2, my - 1.1))
    box.SetEnd(mm(mx[7] + 1.2, my + 1.1))
    box.SetLayer(pcbnew.F_SilkS)
    box.SetWidth(pcbnew.FromMM(0.15))
    b.Add(box)
    text("1", mx[0] - 2.0, my, size=0.8)
    text("CC1101 868 MHz", (mx[0] + mx[7]) / 2, 127.0, size=0.8)
    text("max2mqtt", 113.0, 101.6, size=1.0, layer=pcbnew.B_SilkS)
    text("CC1101 shield v2", 113.0, 103.2, size=0.8, layer=pcbnew.B_SilkS)

    pcbnew.ZONE_FILLER(b).Fill(b.Zones())
    pcbnew.SaveBoard(path, b)


def project():
    template = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/template/kicad.kicad_pro"
    pro = json.load(open(template))
    pro["meta"]["filename"] = f"{NAME}.kicad_pro"
    # JLCPCB's standard 2-layer process, comfortably: v1's header position leaves 0.5 mm to the edge.
    pro["board"]["design_settings"]["rules"] = {
        "min_clearance": 0.2,
        "min_copper_edge_clearance": 0.3,
        "min_track_width": 0.2,
        "min_via_diameter": 0.5,
        "min_through_hole_diameter": 0.3,
        "min_hole_to_hole": 0.25,
        "min_text_height": 0.8,
        "min_text_thickness": 0.12,
    }
    path = os.path.join(HERE, f"{NAME}.kicad_pro")
    json.dump(pro, open(path, "w"), indent=2)


if __name__ == "__main__":
    project()
    footprint_library()
    symbols, _ = schematic()
    board(symbols)
    print("written", NAME)
