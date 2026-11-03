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
    external fun walks(h: Long): String
    external fun setWalks(h: Long, s: String)
    external fun layout(h: Long): String
    external fun setLayout(h: Long, s: String)
    external fun setText(h: Long, text: String)
    external fun commitText(h: Long)
    external fun undo(h: Long, redo: Boolean)
    external fun canUndo(h: Long, redo: Boolean): Boolean
    external fun deleteSelection(h: Long)
    external fun edgeChoice(h: Long, choice: Int)
    external fun createCenter(h: Long)
    external fun stats(h: Long): String
    /** See `NODE_*` in core/src/editor.rs. */
    external fun nodeChoice(h: Long, choice: Int)
    external fun copySelection(h: Long): String?
    external fun selectionText(h: Long): String?
    external fun paste(h: Long, board: String, text: String)
    /** step: 0 new query, 1 next, -1 previous. Returns "current\tcount". */
    external fun search(h: Long, query: String, step: Int): String
    external fun searchClose(h: Long)
    external fun setTop(h: Long, top: Float)
    external fun pathExit(h: Long): Boolean

    // Boards and sync (core library.rs): the shell picks, the core keeps.
    external fun setLibrary(h: Long, dir: String)
    external fun syncReady(h: Long): Boolean
    external fun blank(h: Long): Boolean
    /** Connection string (sync.json contents); false if it is not one. */
    external fun setSync(h: Long, text: String): Boolean
    /** "id	name	open" lines. */
    external fun boards(h: Long): String
    /** Returns the board's name, or null. */
    external fun openBoard(h: Long, id: String): String?
    /** New board from a file's text (empty = blank); its id or "!error". */
    external fun importBoard(h: Long, name: String, text: String): String
    external fun saveBoard(h: Long): Boolean
    /** 1: board changed (redraw); 2: server out of reach. */
    external fun syncPoll(h: Long): Int
}
