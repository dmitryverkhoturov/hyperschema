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
    external fun zoom(h: Long, f: Float, x: Float, y: Float)
    external fun fit(h: Long)
    external fun render(h: Long): Boolean
    external fun load(h: Long, text: String): String?
    external fun editable(h: Long): Boolean
    external fun serialize(h: Long): String
    external fun dragStart(h: Long, x: Float, y: Float): Boolean
    external fun dragMove(h: Long, x: Float, y: Float)
    external fun dragEnd(h: Long): Boolean
    external fun nodeAt(h: Long, x: Float, y: Float): String?
    external fun setText(h: Long, id: String, text: String)
    external fun createAt(h: Long, x: Float, y: Float, text: String)
    external fun delete(h: Long, id: String)
    external fun stats(h: Long): String
}
