package org.androidcontroller.keeper;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.StateListDrawable;
import android.widget.Button;
import android.widget.CompoundButton;

/**
 * The look of Test Bed on a PC (GNOME's libadwaita, dark): its colours,
 * round buttons in its kinds (normal, suggested, destructive, flat, and
 * toggled), and blue switches.
 */
final class Ui {
    private Ui() {}

    // libadwaita's dark palette.
    static final int WINDOW_BG = 0xFF222226;
    static final int HEADER_BG = 0xFF2E2E32;
    /** Cards and boxed lists: white at 8% over the window. */
    static final int CARD_BG = 0xFF343438;
    static final int FG = 0xFFFFFFFF;
    /** Dim labels: the foreground at 55%. */
    static final int DIM = 0xFFA0A0A3;
    static final int LINE = 0x1AFFFFFF;
    static final int ACCENT = 0xFF3584E4;
    static final int ACCENT_TEXT = 0xFF78AEED;
    static final int SUCCESS = 0xFF8FF0A4;
    static final int WARNING = 0xFFF8E45C;
    static final int ERROR = 0xFFFF7B63;
    static final int DESTRUCTIVE = 0xFFC01C28;

    enum Kind {
        /** A button on its own: white at 10%. */
        NORMAL,
        /** The main action (blue), as `suggested-action`. */
        SUGGESTED,
        /** Removing or ending something (red), as `destructive-action`. */
        DESTRUCTIVE,
        /** No background until pressed, as `flat`. */
        FLAT,
        /** A toggle button that is on: white at 30%, as `:checked`. */
        CHECKED,
    }

    /** A round (pill-shaped) button of {@code kind}. */
    static Button button(Context c, String label, Kind kind) {
        Button b = new Button(c);
        b.setText(label);
        b.setAllCaps(false);
        b.setTypeface(Typeface.DEFAULT_BOLD);
        b.setTextSize(14);
        b.setMinWidth(0);
        b.setMinimumWidth(dp(c, 40));
        b.setMinHeight(0);
        b.setMinimumHeight(dp(c, 36));
        b.setPadding(dp(c, 16), 0, dp(c, 16), 0);
        b.setStateListAnimator(null);
        b.setElevation(0);
        style(b, kind);
        return b;
    }

    /** A round button for one symbol (↻, ✕, ☰…), as `circular`. */
    static Button circle(Context c, String symbol, Kind kind) {
        Button b = button(c, symbol, kind);
        b.setPadding(0, 0, 0, 0);
        b.setTypeface(Typeface.DEFAULT);
        b.setTextSize(17);
        b.setMinimumWidth(dp(c, 36));
        return b;
    }

    static void style(Button b, Kind kind) {
        b.setBackground(background(kind));
        b.setTextColor(new ColorStateList(new int[][] {{-android.R.attr.state_enabled}, {}},
                new int[] {0x80FFFFFF, FG}));
    }

    /** Pill-shaped: radius bigger than any button's height. */
    static Drawable background(Kind kind) {
        int base, pressed;
        switch (kind) {
            case SUGGESTED:
                base = ACCENT;
                pressed = 0xFF1C71D8;
                break;
            case DESTRUCTIVE:
                base = DESTRUCTIVE;
                pressed = 0xFFA51D2D;
                break;
            case FLAT:
                base = Color.TRANSPARENT;
                pressed = 0x26FFFFFF;
                break;
            case CHECKED:
                base = 0x4DFFFFFF;
                pressed = 0x66FFFFFF;
                break;
            default:
                base = 0x1AFFFFFF;
                pressed = 0x33FFFFFF;
                break;
        }
        StateListDrawable d = new StateListDrawable();
        d.addState(new int[] {android.R.attr.state_pressed}, pill(pressed));
        d.addState(new int[] {-android.R.attr.state_enabled}, pill(kind == Kind.FLAT ? base : (base & 0x00FFFFFF) | 0x40000000));
        d.addState(new int[] {}, pill(base));
        return d;
    }

    static GradientDrawable pill(int color) {
        return rounded(color, 999);
    }

    static GradientDrawable rounded(int color, int radiusPx) {
        GradientDrawable g = new GradientDrawable();
        g.setColor(color);
        g.setCornerRadius(radiusPx);
        return g;
    }

    /** Switches and check boxes: blue when on, as on the PC. */
    static void style(CompoundButton s) {
        s.setTextColor(FG);
        ColorStateList on = new ColorStateList(new int[][] {{android.R.attr.state_checked}, {}},
                new int[] {ACCENT, 0x40FFFFFF});
        if (s instanceof android.widget.Switch) {
            android.widget.Switch sw = (android.widget.Switch) s;
            sw.setThumbTintList(ColorStateList.valueOf(FG));
            sw.setTrackTintList(on);
            sw.setTrackTintMode(android.graphics.PorterDuff.Mode.SRC);
        } else {
            s.setButtonTintList(new ColorStateList(new int[][] {{android.R.attr.state_checked}, {}},
                    new int[] {ACCENT, DIM}));
        }
    }

    static int dp(Context c, int v) {
        return Math.round(v * c.getResources().getDisplayMetrics().density);
    }
}
