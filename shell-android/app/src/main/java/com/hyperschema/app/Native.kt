package com.hyperschema.app

import android.view.Surface

/** JNI bridge to the Rust core (`android/src/lib.rs`). */
object Native {
    init {
        System.loadLibrary("hs_android")
    }

    external fun create(surface: Surface, w: Int, h: Int, boxes: Int): Long
    external fun attach(h: Long, surface: Surface, w: Int, hgt: Int)
    external fun detach(h: Long)
    external fun resize(h: Long, w: Int, hgt: Int)
    external fun pan(h: Long, dx: Float, dy: Float)
    external fun zoom(h: Long, f: Float, x: Float, y: Float)
    external fun fit(h: Long)
    external fun render(h: Long): Boolean
    external fun load(h: Long, text: String): String?
    external fun stats(h: Long): String
}
