#!/bin/sh
# Regenerate the shield from generate.py, check it, and export what a PCB maker needs.
# Needs KiCad 10 (KICAD points at its app bundle; the default is macOS).
set -eu
cd "$(dirname "$0")"
KICAD=${KICAD:-/Applications/KiCad/KiCad.app}
PY="$KICAD/Contents/Frameworks/Python.framework/Versions/Current/bin/python3"
CLI="$KICAD/Contents/MacOS/kicad-cli"
# Run a step without KiCad's noise; a failing step stops the build, with its full output.
quiet() {
  out=$("$@" 2>&1) || { printf '%s\n' "$out" >&2; echo "failed: $*" >&2; exit 1; }
  printf '%s\n' "$out" | grep -v -e Fontconfig -e 'memory leak' -e wxApp -e '^Plotted to' -e '^Created file' || true
}

quiet "$PY" generate.py
rm -f cc1101-shield.kicad_prl

# Both checks must be clean, including that the board matches the schematic.
"$CLI" sch erc --exit-code-violations -o /tmp/cc1101-shield-erc.rpt cc1101-shield.kicad_sch >/dev/null 2>&1 \
  || { cat /tmp/cc1101-shield-erc.rpt; exit 1; }
"$CLI" pcb drc --schematic-parity --severity-all --exit-code-violations -o /tmp/cc1101-shield-drc.rpt \
  cc1101-shield.kicad_pcb >/dev/null 2>&1 || { cat /tmp/cc1101-shield-drc.rpt; exit 1; }
echo "ERC and DRC clean"

rm -rf fab && mkdir -p fab/gerbers images
quiet "$CLI" pcb export gerbers --no-protel-ext --layers F.Cu,B.Cu,F.Mask,B.Mask,F.Silkscreen,B.Silkscreen,Edge.Cuts \
  -o fab/gerbers/ cc1101-shield.kicad_pcb
quiet "$CLI" pcb export drill --format excellon --excellon-separate-th -o fab/gerbers/ cc1101-shield.kicad_pcb
for f in F_Cu B_Cu F_Mask B_Mask F_Silkscreen B_Silkscreen Edge_Cuts; do
  test -s "fab/gerbers/cc1101-shield-$f.gbr" || { echo "missing $f" >&2; exit 1; }
done
test -s fab/gerbers/cc1101-shield-PTH.drl || { echo "missing the drill file" >&2; exit 1; }
(cd fab/gerbers && zip -q -X ../cc1101-shield-gerbers.zip ./*)
rm -rf fab/gerbers
quiet "$CLI" sch export pdf -o fab/cc1101-shield-schematic.pdf cc1101-shield.kicad_sch
for side in top bottom; do
  quiet "$CLI" pcb render --side "$side" --quality high --width 1000 --height 1100 --zoom 0.8 \
    -o "images/$side.png" cc1101-shield.kicad_pcb
done
echo "fab/cc1101-shield-gerbers.zip, fab/cc1101-shield-schematic.pdf, images/"
