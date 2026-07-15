# 3D-printable enclosure

OpenSCAD source for a Greaseweazle floppy-drive tower. Two parts:

| File | What it is |
|------|------------|
| `weazleEnclosure.scad` | The drive tower — stacks floppy drives above a Meanwell PSU and a panel-mount AC inlet, with a carry handle. **Parametric drive stack.** |
| `greaseweasleShelf.scad` | A shelf/cradle that holds a Greaseweazle **v4.1** board (LED + USB cutouts, labels, logo), bolts to the top-rear of the tower. |

Open either in [OpenSCAD](https://openscad.org), press **F5** to preview / **F6** to
render, then **Export → STL** for your slicer.

## Customising the drive tower

The stack is driven by one line near the top of `weazleEnclosure.scad`:

```scad
bays = [ d525h(), d525h(), d35() ];   // default: two half-height 5.25" + one 3.5"
```

It's read bottom-to-top. Mix and match these bay types:

- `d525h()` — half-height 5.25" bay (the common floppy drive)
- `d525f()` — **full-height** 5.25" bay (twice as tall)
- `d35()` — 3.5" bay

Add, remove, or reorder entries freely — the shell, PSU position, inlet, handle
and board mount all resize to follow. Examples:

```scad
bays = [ d525h() ];                    // a single half-height drive
bays = [ d525f(), d35() ];             // one full-height 5.25" + one 3.5"
bays = [ d525h(), d525h(), d525h() ];  // three half-height 5.25" drives
```

The tower keeps a 5.25" footprint width whatever you put in it; 3.5" drives
mount within that width with their screw counterbores reaching the outer wall.

Every part's dimensions live in labelled vectors near the top (`driveBody`,
`driveBody35`, `meanwell`, `plugin`, …) — measure your hardware and edit them
to fit different drives or a different PSU.

## Notes

- `greaseweasleShelf.scad` imports **`weazle.svg`** (the logo). Keep that file
  next to the `.scad` or the logo cut is silently skipped (everything else
  still renders).
- Both files support multi-material export: set `current_color` to `"gray"` /
  `"white"` / `"black"` to export just that colour's geometry for a multi-tone
  print; leave it `"ALL"` for a single-body STL.
