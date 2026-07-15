// =============================================================================
//  The Lube Shop — Greaseweazle v4.1 board shelf / cradle
// =============================================================================
//
//  A small open-front shelf that holds a Greaseweazle v4.1 board on four
//  stand-off ring posts, with panel cutouts lined up to the board's ACT/PWR
//  LEDs and USB connector, plus front-panel labels and the Greaseweazle logo.
//  It bolts onto the top-rear of the drive tower (weazleEnclosure.scad).
//
//  NOTE: the logo uses import("weazle.svg"). That SVG must sit next to this
//  file or the logo cut is silently skipped (everything else still renders).
//  Include weazle.svg when sharing this model.
// =============================================================================

$fn = 60;

// -----------------------------------------------------------------------------
//  Outer shell (mm).
// -----------------------------------------------------------------------------
shelfX = 146;         // width
shelfY = 52;          // depth (board sits inside)
shelfZ = 38.5;        // height
shelfThickness = 5;   // wall / floor thickness

// -----------------------------------------------------------------------------
//  Board stand-off ring posts — a 2x2 grid the board screws down onto.
// -----------------------------------------------------------------------------
ringOd = 10;          // post outer diameter
ringId = 2.6;         // screw-clearance bore
ringX = 81;           // post spacing in X (hole centre-to-centre)
ringY = 35;           // post spacing in Y
ringZ = 2;            // post height (board stand-off)
ringOffset = 9.5;     // grid offset from the front inner wall

// -----------------------------------------------------------------------------
//  Front-panel cutouts (mm).
// -----------------------------------------------------------------------------
ledholes = [41.5, 50, 8, 5];  // 0 first hole X, 1 second-hole X (from left mount hole),
                              // 2 height above board bottom, 3 hole diameter
usbHole  = [7.5, 5, 68, 3];   // 0 slot diameter, 1 slot spread (hull length),
                              // 2 X distance from left mount hole, 3 height
sideHoles = [9.2, 29.57, 9.7, 2.6]; // 0 hole 1 Z, 1 hole 2 Z, 2 Y position, 3 diameter

textThickness = .4;   // engraved/raised label + logo depth

// -----------------------------------------------------------------------------
//  Multi-material export (see weazleEnclosure.scad). "ALL" = single body.
// -----------------------------------------------------------------------------
current_color = "ALL";
//current_color = "gray";
//current_color = "black";
//current_color = "white";

// =============================================================================
//  HELPERS
// =============================================================================

/* Like color(), but for multi-material export: children of a non-selected
 * colour are dropped when `current_color` names a single colour. */
module multicolor(color) {
    if (current_color != "ALL" && current_color != color) {
        // drop these children
    } else {
        color(color) children();
    }
}

/* A single stand-off ring post: an OD cylinder with an ID screw bore. */
module mountRing(od, id, z) {
    difference() {
        linear_extrude(height = z) circle(d = od);
        linear_extrude(height = z) circle(d = id);
    }
}

/* The four front-panel labels and the Greaseweazle logo, as flat extrusions.
 * Used both raised (positive) and as a cut (negative) for a two-tone inlay. */
module frontGraphics() {
    translate([10, textThickness, 23])   rotate([90, 0, 0]) linear_extrude(textThickness) text("Greaseweazle v4.1", font = "Liberation Sans:style=Bold Italic");
    translate([69, textThickness, 9])    rotate([90, 0, 0]) linear_extrude(textThickness) text("ACT", font = "Liberation Sans:style=Bold Italic", size = 3);
    translate([78.5, textThickness, 9])  rotate([90, 0, 0]) linear_extrude(textThickness) text("PWR", font = "Liberation Sans:style=Bold Italic", size = 3);
    translate([96, textThickness, 2])    rotate([90, 0, 0]) linear_extrude(textThickness) text("USB", font = "Liberation Sans:style=Bold Italic", size = 3);
    translate([10, textThickness, -3.5]) rotate([90, 0, 0]) linear_extrude(textThickness) scale(.14) import("weazle.svg");
}

// =============================================================================
//  SOLID BODY (positive) and CUTOUTS (negative)
// =============================================================================

module positive() {
    // Solid outer block.
    multicolor(color = "gray")
        linear_extrude(height = shelfZ) square([shelfX, shelfY + shelfThickness]);

    // Raised labels + logo (white, for two-material prints).
    multicolor(color = "white") frontGraphics();
}

module negative() {
    multicolor(color = "gray") {
        // Hollow out the interior (open front/back).
        translate([shelfThickness, shelfThickness, shelfThickness])
            linear_extrude(height = shelfZ)
                square([shelfX - shelfThickness * 2, shelfY + shelfThickness]);

        // ACT / PWR LED holes (two, spaced ledholes[0] -> ledholes[1]).
        translate([(shelfX - ringX) / 2, -.25, shelfThickness + ringZ + ledholes[2]])
            rotate([-90, 0, 0])
                for (i = [ledholes[0] : ledholes[1] - ledholes[0] : ledholes[1]])
                    translate([i, 0, 0])
                        linear_extrude(height = shelfThickness + .5) circle(d = ledholes[3]);

        // USB slot: a rounded slot made by hulling two bores.
        translate([(shelfX - ringX) / 2 + usbHole[2], -.25, shelfThickness + ringZ + usbHole[3]])
            rotate([-90, 0, 0])
                hull()
                    for (i = [-(usbHole[1] / 2) : usbHole[1] : usbHole[1]])
                        translate([i, 0, 0])
                            linear_extrude(height = shelfThickness + .5) circle(d = usbHole[0]);

        // Side holes (two heights), running through in X.
        translate([0, sideHoles[2], 0])
            rotate([0, 90, 0])
                for (i = [sideHoles[0] : sideHoles[1] - sideHoles[0] : sideHoles[1]])
                    translate([-i, 0, -.25])
                        linear_extrude(height = shelfX + .5) circle(d = sideHoles[3]);

        // Board mount screw holes through the floor (2x2 grid under the posts).
        translate([(shelfX - ringX) / 2, ringOffset + shelfThickness, 0])
            for (i = [0 : ringX : ringX])
                for (j = [0 : ringY : ringY])
                    translate([i, j, 0])
                        linear_extrude(height = shelfThickness) circle(d = ringId);

        // Cut the labels + logo so they read as recesses / two-tone inlay.
        frontGraphics();
    }
}

// =============================================================================
//  BUILD
// =============================================================================

difference() {
    positive();
    negative();
}

// Stand-off ring posts on the floor (2x2 grid).
multicolor(color = "gray")
    translate([(shelfX - ringX) / 2, ringOffset + shelfThickness, shelfThickness])
        for (i = [0 : ringX : ringX])
            for (j = [0 : ringY : ringY])
                translate([i, j, 0])
                    mountRing(od = ringOd, id = ringId, z = ringZ);
