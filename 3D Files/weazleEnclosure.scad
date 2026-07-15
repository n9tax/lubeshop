// =============================================================================
//  The Lube Shop — Greaseweazle drive tower enclosure
// =============================================================================
//
//  A print-in-place tower that stacks floppy drives above a Meanwell PSU and a
//  panel-mount AC inlet, with a carry handle on top. The front of the drive
//  bays is closed; drives slide in from the rear and are held by side screws.
//
//  ---------------------------------------------------------------------------
//  TO CHANGE THE DRIVE STACK: edit the `bays = [...]` line in the DRIVE STACK
//  section below (after the dimension specs). It is read bottom-to-top; each
//  entry is one bay:
//
//      d525h()   half-height 5.25" bay   (the common floppy drive)
//      d525f()   FULL-height 5.25" bay   (twice as tall)
//      d35()     3.5" bay
//
//  Add, remove, or reorder entries freely — the outer shell, the PSU position,
//  the inlet, the handle and the Greaseweazle mount all move to follow the new
//  total height. Examples:
//
//      bays = [ d525h(), d525h(), d35() ];   // the original design (default)
//      bays = [ d525h() ];                   // a single half-height drive
//      bays = [ d525f(), d35() ];            // one full-height 5.25" + one 3.5"
//      bays = [ d525h(), d525h(), d525h() ]; // three half-height 5.25" drives
//
//  The tower keeps a 5.25" footprint width regardless of what you put in it;
//  3.5" drives mount centred within that width (screw counterbores reach the
//  outer wall so they stay accessible).
// =============================================================================

$fn = 50;

// -----------------------------------------------------------------------------
//  Multi-material export.  Leave "ALL" for a normal single-body STL. Set to a
//  colour name to export only the parts of that colour (for multi-colour /
//  multi-material printing).
// -----------------------------------------------------------------------------
current_color = "ALL";
//current_color = "black";
//current_color = "white";
//current_color = "gray";

// =============================================================================
//  COMPONENT DIMENSIONS  (spec sheets — mm)
//
//  Each vector is one physical part. The index legend after `//` names every
//  field; the code refers to them by index (e.g. driveBody[6] = mounting-hole
//  diameter). Measure a real part and edit these to fit different hardware.
// =============================================================================

// 5.25" half-height drive bay.
driveBody = [147, 43, 200, 10.15, 52.5, 131.6, 4.2, 6, 3, 3, 8, .4];
//            0    1   2    3      4     5      6    7  8  9  10 11
//  0 width          1 height (half-height)   2 depth
//  3 screw height from drive bottom          4 screw 1 Y (from front face)
//  5 screw 2 Y (from front face)             6 mounting-hole diameter
//  7 screw-head diameter                     8 screw-head recess depth
//  9 front-bezel relief width (per side)    10 front-bezel relief depth
// 11 front-bezel relief height (per side)

// 3.5" drive bay.
driveBody35 = [102.5, 26, 143.4, 5.35, 25.42, 85.52, 4.2, 6, 3];
//              0      1   2      3     4      5      6    7  8
//  0 width   1 height   2 depth   3 screw height from bottom
//  4 screw 1 Y   5 screw 2 Y   6 hole dia   7 head dia   8 head recess depth

// Meanwell Mean Well RD-50A-VP open-frame PSU. https://a.co/d/091MpEGP < Amazon link
meanwell = [98, 36.5, 100, 46, 20.75, 76, 18.7, 18, 91.5, 4.2, 6, 3];
//           0   1     2    3   4      5   6     7   8     9    10 11
//  0 width  1 height  2 depth
//  3 top-screw X   4 top-screw Y1   5 top-screw Y2 (from terminal side)
//  6 side-screw height from bottom  7 side-screw Y1  8 side-screw Y2
//  9 mounting-hole dia  10 screw-head dia  11 screw-head recess depth

// Panel-mount AC inlet (IEC-style) with mounting flange. https://a.co/d/0dBkvaSE < Amazon link
plugin = [28, 47.5, 50, 49, 60, 3.3, 40, 5.5, 2.5];
//         0   1     2   3   4   5    6   7    8
//  0 body X   1 body Z   2 body Y   3 flange X   4 flange Z   5 flange Y
//  6 screw centre-to-centre   7 captive-nut across-flats   8 nut thickness

// Carry handle.
handle = [25, 40]; // 0 rod diameter, 1 stand-off height

// Wall / floor / divider thickness used everywhere.
wallThickness = 5;

// =============================================================================
//  DRIVE-BAY VOCABULARY  — the helpers the `bays` list is built from.
//
//  Each returns [type, cavityHeight]. `type` selects the cutout geometry;
//  `cavityHeight` is how much vertical space the bay occupies.
// =============================================================================

H_525_HALF = driveBody[1];                    // half-height 5.25" cavity (43)
H_525_FULL = 82.55 + (driveBody[1] - 41.3);   // full-height 5.25", same clearance
H_35       = driveBody35[1];                   // 3.5" cavity (26)

function d525h() = ["525", H_525_HALF];
function d525f() = ["525", H_525_FULL];
function d35()   = ["35",  H_35];

// =============================================================================
//  THE DRIVE STACK  — edit this line (see the header for the vocabulary).
// =============================================================================
bays = [ d525h(), d525h(), d35() ];   // ORIGINAL (featured) — two half-height 5.25" + one 3.5"
//bays = [ d525f(), d35() ];          // one full-height 5.25" + one 3.5"
//bays = [ d525h(), d35() ];          // one half-height 5.25" + one 3.5"

// =============================================================================
//  DERIVED GEOMETRY  — computed from `bays`; nothing below needs hand-editing.
// =============================================================================

nDrives   = len(bays);                              // number of drive bays
outerW    = driveBody[0] + wallThickness * 2;       // tower width (5.25" footprint)
bodyDepth = driveBody[2];                            // tower depth (deepest drive)

// Total cavity height of bays 0..n-1 (recursive sum; SCAD has no fold).
function heightBelow(n) = n <= 0 ? 0 : bays[n - 1][1] + heightBelow(n - 1);

// Z of the floor of drive bay i: one wall per bay below it, plus their heights.
function zDrive(i) = wallThickness * (i + 1) + heightBelow(i);

totalDriveH = heightBelow(nDrives);                  // all drive cavities summed
zPSU        = wallThickness * (nDrives + 1) + totalDriveH;   // PSU compartment floor
totalZ      = zPSU + meanwell[1] + wallThickness;    // overall tower height

// Height the closed front face rises to (covers the drive bays only, matching
// the original half-wall overlap onto the PSU divider).
frontFaceH  = totalDriveH + wallThickness * (nDrives + 0.5);

// =============================================================================
//  REUSABLE GEOMETRY HELPERS
// =============================================================================

/* Like color(), but for multi-material export: when `current_color` names one
 * colour, children of any other colour are dropped from the model. */
module multicolor(color) {
    if (current_color != "ALL" && current_color != color) {
        // drop these children
    } else {
        color(color) children();
    }
}

/* A rectangular tube standing in Z, open on the front and back (Y) faces —
 * walls on the left/right and top/bottom only. Used for the outer shell. */
module emptyBox(x, y, z, thickness) {
    difference() {
        linear_extrude(z) square([x, y]);
        translate([thickness, -.25, thickness])
            linear_extrude(z - thickness * 2) square([x - thickness * 2, y + .5]);
    }
}

/* A screw passage drawn along the extrude (Z) axis for the caller to rotate
 * into place: a through shank with a head recess at each end. `farHeadExtra`
 * lengthens the far recess so a narrow part's screw can still be reached from
 * the wider outer wall (used for 3.5" drives inside the 5.25" shell). */
module screwHole(shankLen, holeDia, headDia, headDepth, farHeadExtra = 0) {
    linear_extrude(shankLen) circle(d = holeDia);                       // shank
    linear_extrude(headDepth) circle(d = headDia);                     // near head
    translate([0, 0, shankLen - headDepth])
        linear_extrude(headDepth + farHeadExtra) circle(d = headDia);  // far head
}

/* Carry handle: a vertical stand-off with a horizontal grip rod across the top,
 * supported at both ends. `len` is the grip length, `dia` the rod diameter,
 * `height` the stand-off length. */
module handle(len, dia, height) {
    linear_extrude(height = len) circle(d = dia);
    translate([0, 0, len - dia * 1.7]) rotate([0, 90, 0]) linear_extrude(height) circle(d = dia);
    translate([0, 0, dia * 1.7])       rotate([0, 90, 0]) linear_extrude(height) circle(d = dia);
}

// =============================================================================
//  DRIVE-BAY CUTOUTS
// =============================================================================

/* Dispatch one bay's cutout to the right geometry at floor height `zOff`. */
module driveBay(bay, zOff) {
    if      (bay[0] == "525") bay525(bay[1], zOff);
    else if (bay[0] == "35")  bay35(bay[1], zOff);
}

/* A 5.25" bay of cavity height `h` at floor `zOff`: the drive pocket, the front
 * bezel relief, and the two side mounting screws (one row, from the drive
 * bottom). Works for both half- and full-height drives — full height is just a
 * taller `h`. Note: full-height 5.25" drives often also have an UPPER screw
 * row; if you need it, add a second `screwHole` at `driveBody[3] + zOff + <dz>`. */
module bay525(h, zOff) {
    // Drive pocket.
    translate([wallThickness, 0, zOff])
        linear_extrude(h) square([driveBody[0], driveBody[2]]);

    // Front-bezel relief (a shallow lip so the drive face sits flush).
    translate([wallThickness - driveBody[9] / 2, 0, zOff - driveBody[11] / 2])
        linear_extrude(h + driveBody[11])
            square([driveBody[0] + driveBody[9], driveBody[10]]);

    // Two side screws, at the drive's spec height, running across the width.
    for (y = [driveBody[4], driveBody[5]])
        translate([0, y, driveBody[3] + zOff]) rotate([0, 90, 0])
            screwHole(outerW, driveBody[6], driveBody[7], driveBody[8]);
}

/* A 3.5" bay at floor `zOff`. Narrower than the shell, so the far screw-head
 * recess is extended out to the outer wall to stay reachable. */
module bay35(h, zOff) {
    // Drive pocket (against the shared left wall; sits within the 5.25" width).
    translate([wallThickness, 0, zOff])
        linear_extrude(h) square([driveBody35[0], driveBody35[2]]);

    // Two side screws; far recess reaches from the drive out to the outer wall.
    farExtra = outerW - (driveBody35[0] + wallThickness * 2);
    for (y = [driveBody35[4], driveBody35[5]])
        translate([0, y, driveBody35[3] + zOff]) rotate([0, 90, 0])
            screwHole(driveBody35[0] + wallThickness * 2,
                      driveBody35[6], driveBody35[7], driveBody35[8], farExtra);
}

// =============================================================================
//  PSU + INLET + MOUNT CUTOUTS
// =============================================================================

/* Meanwell PSU pocket with top- and side-mount screws, at compartment floor. */
module psu(zOff) {
    translate([wallThickness, driveBody[2] - meanwell[2], zOff]) {
        // Body pocket.
        linear_extrude(meanwell[1]) square([meanwell[0], meanwell[2]]);

        // Side-mount screws through the left wall (shank + head recess).
        for (y = [meanwell[7], meanwell[8]])
            translate([-wallThickness, y, meanwell[6]]) rotate([0, 90, 0]) {
                linear_extrude(meanwell[0] + wallThickness) circle(d = meanwell[9]);
                linear_extrude(meanwell[11]) circle(d = meanwell[10]);
            }

        // Top-mount screws up through the divider (shank + recessed head).
        for (y = [meanwell[4], meanwell[5]]) {
            translate([meanwell[3], y, 0])
                linear_extrude(meanwell[1] + wallThickness) circle(d = meanwell[9]);
            translate([meanwell[3], y, meanwell[1] - meanwell[11] + wallThickness])
                linear_extrude(meanwell[11]) circle(d = meanwell[10]);
        }
    }
}

/* Panel-mount AC inlet: body aperture, two flange screws with captive-nut
 * pockets, and the flange recess. Sits centred over the PSU compartment. */
module inlet() {
    zPlug = zPSU - wallThickness + (meanwell[1] / 2 - plugin[1] / 2) - 2;
    xPlug = meanwell[2] + ((driveBody[0] - meanwell[0] - plugin[0]) / 2 + wallThickness / 2);
    translate([xPlug, driveBody[2] - plugin[2], zPlug]) {
        // Body aperture.
        linear_extrude(plugin[1]) square([plugin[0], plugin[2]]);

        // Flange screws with hex captive-nut pockets, one each side.
        for (x = [-(plugin[6] / 2 - plugin[0] / 2), plugin[0] + plugin[6] / 2 - plugin[0] / 2])
            translate([x, plugin[2], plugin[1] / 2]) rotate([90, 0, 0]) {
                linear_extrude(wallThickness * 2) circle(d = meanwell[9]);
                translate([0, 0, wallThickness * 2 - plugin[8]])
                    linear_extrude(plugin[8]) circle(d = plugin[7], $fn = 6);
            }

        // Flange recess.
        translate([plugin[0] / 2 - plugin[3] / 2, plugin[2], plugin[1] / 2 - plugin[4] / 2])
            linear_extrude(plugin[4]) square([plugin[3], plugin[5]]);
    }
}

/* Two screw holes in the top-rear for mounting the Greaseweazle shelf. */
module weazleMount() {
    for (down = [10, 30])
        translate([0, 10, totalZ - (wallThickness + down)]) rotate([0, 90, 0])
            screwHole(outerW, driveBody[6], driveBody[7], driveBody[8]);
}

// =============================================================================
//  SOLID BODY  (positive) and CUTOUTS (negative)
// =============================================================================

module positive() {
    multicolor(color = "gray") {
        // Closed front face over the drive bays.
        linear_extrude(frontFaceH) square([outerW, driveBody35[2]]);

        // Outer shell (open front/back tube).
        emptyBox(outerW, driveBody[2], totalZ, wallThickness);

        // Back support wall for the inlet, near the top of the PSU compartment.
        translate([0, driveBody[2] - wallThickness * 2, totalZ - (plugin[4] + wallThickness)])
            linear_extrude(plugin[4] + wallThickness)
                square([outerW, wallThickness * 2]);

        // Carry handle across the top.
        translate([outerW / 2, 0, totalZ + handle[1]]) rotate([0, 90, 90])
            handle(driveBody[2], handle[0], handle[1]);
    }
}

module negative() {
    multicolor(color = "red") {
        // Every drive bay, stacked bottom-to-top from the `bays` list.
        for (i = [0 : nDrives - 1]) driveBay(bays[i], zDrive(i));

        // PSU, AC inlet and Greaseweazle mount above the drives.
        psu(zPSU);
        inlet();
        weazleMount();
    }
}

// =============================================================================
//  BUILD
// =============================================================================

difference() {
    positive();
    negative();
}
