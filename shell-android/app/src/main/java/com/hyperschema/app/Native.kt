package com.hyperschema.app

import android.view.Surface

/** JNI bridge to the Rust core (`android/src/lib.rs`). */
object Native {
    init {
        System.loadLibrary("hs_android")
    }

    external fun create(surface: Surface, w: Int, h: Int, density: Float): Long
    external fun attach(h: Long, surface: Surface, w: Int, hgt: Int)
    external fun detach(h: Long)
    external fun resize(h: Long, w: Int, hgt: Int)
    external fun pan(h: Long, dx: Float, dy: Float)
    external fun editSelected(h: Long)
    external fun zoom(h: Long, f: Float, x: Float, y: Float)
    external fun fit(h: Long)
    external fun setNested(h: Long, on: Boolean)
    external fun render(h: Long): Boolean
    /** action: 0 down, 1 move, 2 up, 3 cancel; t: event time, ms. */
    external fun touch(h: Long, action: Int, id: Long, x: Float, y: Float, t: Long)
    external fun tick(h: Long, t: Long): Boolean
    external fun poll(h: Long): String
    external fun load(h: Long, text: String): String?
    external fun reload(h: Long, text: String): Boolean
    external fun editable(h: Long): Boolean
    external fun serialize(h: Long): String
    external fun setText(h: Long, text: String)
    external fun commitText(h: Long)
    external fun undo(h: Long, redo: Boolean)
    external fun canUndo(h: Long, redo: Boolean): Boolean
    external fun deleteSelection(h: Long)
    external fun edgeChoice(h: Long, choice: Int)
    external fun createCenter(h: Long)
    external fun stats(h: Long): String
}
