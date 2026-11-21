package org.geppetto.carlo.fixture;

import android.graphics.Canvas;
import android.graphics.ColorFilter;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.graphics.drawable.Drawable;

/**
 * Visual marker of the app under test: two full-width bands of eight equal
 * vertical stripes, the second with the sequence reversed, on white.
 *
 * It is a background only, so it adds no view and no accessibility node. The
 * sequence and geometry are mirrored by tools/smoke_display_mirror.py.
 */
final class MarkerDrawable extends Drawable {
    /** red, green, blue, yellow, magenta, cyan, red, blue: fully saturated, opaque. */
    static final int[] STRIPES = {
        0xFFFF0000, 0xFF00FF00, 0xFF0000FF, 0xFFFFFF00,
        0xFFFF00FF, 0xFF00FFFF, 0xFFFF0000, 0xFF0000FF,
    };
    /** Each band is 1/BAND of the height; the white gap between them 1/GAP. */
    static final int BAND = 10;
    static final int GAP = 40;

    private final Paint paint = new Paint();

    @Override public void draw(Canvas canvas) {
        Rect bounds = getBounds();
        paint.setAntiAlias(false);
        paint.setStyle(Paint.Style.FILL);
        paint.setColor(0xFFFFFFFF);
        canvas.drawRect(bounds, paint);
        int band = bounds.height() / BAND;
        int second = bounds.top + band + bounds.height() / GAP;
        int count = STRIPES.length;
        for (int index = 0; index < count; index++) {
            int left = bounds.left + bounds.width() * index / count;
            int right = bounds.left + bounds.width() * (index + 1) / count;
            paint.setColor(STRIPES[index]);
            canvas.drawRect(left, bounds.top, right, bounds.top + band, paint);
            paint.setColor(STRIPES[count - 1 - index]);
            canvas.drawRect(left, second, right, second + band, paint);
        }
    }

    @Override public void setAlpha(int alpha) { }
    @Override public void setColorFilter(ColorFilter filter) { }
    @Override public int getOpacity() { return PixelFormat.OPAQUE; }
}
