package com.hyperschema.app

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.view.Choreographer
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.VelocityTracker
import android.view.WindowManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.TextView
import android.widget.Toast
import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.hypot

/**
 * Thin shell: a SurfaceView the Rust engine renders into, frames driven by
 * Choreographer only while something moves.
 *
 * Touch (Freeform-like): one finger on empty canvas pans, on a concept
 * moves it (it nests where its centre lands); two fingers pinch; double
 * tap on a concept edits its text, on empty canvas makes a new one; double
 * tap and drag zooms; long press on a concept offers edit / delete.
 * Changes save themselves half a second later into the opened file.
 */
class MainActivity : Activity(), SurfaceHolder.Callback, Choreographer.FrameCallback {
    private var handle = 0L
    private var creating = false
    private var attached = false
    private lateinit var surfaceView: SurfaceView
    private lateinit var title: TextView
    private var scheduled = false
    private var dirty = false
    private var pendingText: String? = null
    private var uri: Uri? = null
    private val ui = Handler(Looper.getMainLooper())
    private val saver = Runnable { save() }

    // Gesture state: centroid and mean distance to it over all fingers.
    private var lastCx = 0f
    private var lastCy = 0f
    private var lastSpan = 0f
    private var lastCount = 0
    private var velocity: VelocityTracker? = null
    private var flingVx = 0f
    private var flingVy = 0f
    private var lastFrameNs = 0L
    private var dragging = false

    // Double tap: a tap, or a zoom if the second touch drags.
    private var inDoubleTap = false
    private var dtZoomed = false
    private var dtX = 0f
    private var dtY = 0f
    private var dtLastY = 0f

    private lateinit var taps: GestureDetector

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        window.addFlags(WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS)
        window.statusBarColor = Color.rgb(247, 246, 243)
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        val ink = Color.rgb(42, 42, 42)
        val menu = TextView(this).apply {
            text = "☰"
            textSize = 22f
            setTextColor(ink)
            setPadding(36, 12, 28, 12)
            setOnClickListener { showMenu(this) }
        }
        title = TextView(this).apply {
            textSize = 14f
            setTextColor(Color.rgb(85, 85, 85))
            text = ""
        }
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(menu)
            addView(title)
        }
        val root = FrameLayout(this)
        root.addView(surfaceView)
        root.addView(bar, FrameLayout.LayoutParams(-1, -2, Gravity.TOP))
        setContentView(root)
        taps = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onDoubleTapEvent(e: MotionEvent): Boolean {
                when (e.actionMasked) {
                    MotionEvent.ACTION_DOWN -> {
                        inDoubleTap = true
                        dtZoomed = false
                        dtX = e.x
                        dtY = e.y
                        dtLastY = e.y
                    }
                    MotionEvent.ACTION_MOVE -> {
                        if (abs(e.y - dtY) > 24f) dtZoomed = true
                        if (dtZoomed && handle != 0L) {
                            Native.zoom(handle, exp((e.y - dtLastY) * 0.006f), dtX, dtY)
                            request()
                        }
                        dtLastY = e.y
                    }
                    MotionEvent.ACTION_UP -> {
                        if (!dtZoomed) doubleTap(e.x, e.y)
                        inDoubleTap = false
                    }
                    MotionEvent.ACTION_CANCEL -> inDoubleTap = false
                }
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                longPress(e.x, e.y)
            }
        })
        surfaceView.setOnTouchListener { _, ev -> onTouch(ev); true }
        if (!readIntent(intent)) {
            getPreferences(MODE_PRIVATE).getString("last", null)?.let { open(Uri.parse(it)) }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        readIntent(intent)
    }

    override fun onPause() {
        super.onPause()
        if (ui.hasCallbacks(saver)) {
            ui.removeCallbacks(saver)
            save()
        }
    }

    /** Opens what another app handed us; false if nothing came. */
    private fun readIntent(intent: Intent?): Boolean {
        val u: Uri? = when (intent?.action) {
            Intent.ACTION_VIEW -> intent.data
            @Suppress("DEPRECATION")
            Intent.ACTION_SEND -> intent.getParcelableExtra(Intent.EXTRA_STREAM)
            else -> null
        }
        if (u != null) {
            open(u)
            return true
        }
        val text = intent?.getStringExtra(Intent.EXTRA_TEXT) ?: return false
        uri = null
        pendingText = text
        title.text = "текст из «Поделиться»"
        loadPending()
        return true
    }

    private fun open(u: Uri) {
        val text = runCatching { contentResolver.openInputStream(u)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()
        if (text == null) {
            Toast.makeText(this, "Не открылось", Toast.LENGTH_LONG).show()
            return
        }
        uri = u
        runCatching { contentResolver.takePersistableUriPermission(u, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION) }
        getPreferences(MODE_PRIVATE).edit().putString("last", u.toString()).apply()
        title.text = displayName(u)
        pendingText = text
        loadPending()
    }

    private fun displayName(u: Uri): String {
        val name = runCatching {
            contentResolver.query(u, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c -> if (c.moveToFirst()) c.getString(0) else null }
        }.getOrNull() ?: u.lastPathSegment ?: ""
        return name.removeSuffix(".md").removeSuffix(".excalidraw").removeSuffix(".hyperschema")
    }

    private fun loadPending() {
        val text = pendingText ?: return
        if (handle == 0L) return
        pendingText = null
        val err = Native.load(handle, text)
        if (err != null) Toast.makeText(this, "Не открылось: $err", Toast.LENGTH_LONG).show()
        else if (!Native.editable(handle)) Toast.makeText(this, "Excalidraw — только просмотр. Правка — в .hyperschema (☰ → Открыть)", Toast.LENGTH_LONG).show()
        request()
    }

    private fun changed() {
        request()
        ui.removeCallbacks(saver)
        ui.postDelayed(saver, 500)
    }

    private fun save() {
        val u = uri ?: return
        if (handle == 0L || !Native.editable(handle)) return
        val text = Native.serialize(handle)
        val ok = runCatching { contentResolver.openOutputStream(u, "wt")?.use { it.write(text.toByteArray(Charsets.UTF_8)) } != null }.getOrDefault(false)
        if (!ok) Toast.makeText(this, "Не сохраняется: нет доступа к файлу. Откройте его через ☰ → Открыть", Toast.LENGTH_LONG).show()
    }

    private fun showMenu(anchor: android.view.View) {
        val m = PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Открыть…")
        m.menu.add(0, 2, 1, "Новая схема…")
        m.menu.add(0, 3, 2, "Вписать всё")
        m.menu.add(0, 4, 3, "Сведения")
        m.setOnMenuItemClickListener {
            when (it.itemId) {
                1 -> startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"), REQ_OPEN)
                2 -> startActivityForResult(
                    Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("application/octet-stream")
                        .putExtra(Intent.EXTRA_TITLE, "Новая схема.hyperschema"),
                    REQ_NEW
                )
                3 -> if (handle != 0L) {
                    Native.fit(handle)
                    request()
                }
                4 -> if (handle != 0L) Toast.makeText(this, Native.stats(handle), Toast.LENGTH_LONG).show()
            }
            true
        }
        m.show()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val u = data?.data ?: return
        if (resultCode != RESULT_OK) return
        when (requestCode) {
            REQ_OPEN -> open(u)
            REQ_NEW -> {
                runCatching { contentResolver.openOutputStream(u, "wt")?.use { it.write(EMPTY.toByteArray()) } }
                open(u)
            }
        }
    }

    private fun askText(titleText: String, initial: String, done: (String) -> Unit) {
        val input = EditText(this).apply {
            setText(initial)
            setSelection(initial.length)
        }
        val box = FrameLayout(this).apply {
            setPadding(48, 16, 48, 0)
            addView(input)
        }
        val d = AlertDialog.Builder(this).setTitle(titleText).setView(box)
            .setPositiveButton("Готово") { _, _ -> done(input.text.toString()) }
            .setNegativeButton("Отмена", null).create()
        d.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE)
        d.show()
        input.requestFocus()
    }

    private fun doubleTap(x: Float, y: Float) {
        if (handle == 0L || !Native.editable(handle)) return
        val hit = Native.nodeAt(handle, x, y)
        if (hit != null && isGrab(x, y)) {
            val id = hit.substringBefore('\n')
            askText("Текст", hit.substringAfter('\n')) {
                Native.setText(handle, id, it)
                changed()
            }
        } else {
            askText("Новый концепт", "") {
                if (it.isNotBlank()) {
                    Native.createAt(handle, x, y, it)
                    changed()
                }
            }
        }
    }

    /** Whether (x, y) is on something movable (not a parent's empty inside). */
    private fun isGrab(x: Float, y: Float): Boolean {
        if (!Native.dragStart(handle, x, y)) return false
        Native.dragEnd(handle)
        return true
    }

    private fun longPress(x: Float, y: Float) {
        if (handle == 0L || !Native.editable(handle) || !isGrab(x, y)) return
        val hit = Native.nodeAt(handle, x, y) ?: return
        val id = hit.substringBefore('\n')
        AlertDialog.Builder(this).setItems(arrayOf("Изменить текст", "Удалить")) { _, which ->
            if (which == 0) {
                askText("Текст", hit.substringAfter('\n')) {
                    Native.setText(handle, id, it)
                    changed()
                }
            } else {
                Native.delete(handle, id)
                changed()
            }
        }.show()
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {
        when {
            handle == 0L && !creating -> {
                creating = true
                val surface = holder.surface
                val density = resources.displayMetrics.density
                Thread {
                    val hnd = Native.create(surface, w, h, density)
                    runOnUiThread {
                        handle = hnd
                        creating = false
                        attached = hnd != 0L
                        loadPending()
                        request()
                    }
                }.start()
            }
            handle != 0L && !attached -> {
                Native.attach(handle, holder.surface, w, h)
                attached = true
            }
            handle != 0L -> Native.resize(handle, w, h)
        }
        request()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        if (handle != 0L) Native.detach(handle)
        attached = false
    }

    private fun request() {
        dirty = true
        if (!scheduled) {
            scheduled = true
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    override fun doFrame(frameTimeNanos: Long) {
        scheduled = false
        if (handle == 0L || !attached) return
        val dt = if (lastFrameNs == 0L) 0.016f else ((frameTimeNanos - lastFrameNs) / 1e9f).coerceIn(0f, 0.05f)
        lastFrameNs = frameTimeNanos
        if (flingVx != 0f || flingVy != 0f) {
            Native.pan(handle, flingVx * dt, flingVy * dt)
            // Short glide (~0.25 s time constant), no long drift.
            val k = exp(-dt / 0.25f)
            flingVx *= k
            flingVy *= k
            if (hypot(flingVx, flingVy) < 30f) {
                flingVx = 0f
                flingVy = 0f
            }
            dirty = true
        }
        if (dirty) {
            dirty = false
            Native.render(handle)
            if (flingVx != 0f || flingVy != 0f) request()
        }
    }

    private fun onTouch(ev: MotionEvent) {
        taps.onTouchEvent(ev)
        if (handle == 0L) return
        if (inDoubleTap) {
            lastCount = 0
            return
        }
        val vt = velocity ?: VelocityTracker.obtain().also { velocity = it }
        vt.addMovement(ev)
        val action = ev.actionMasked
        val skip = if (action == MotionEvent.ACTION_POINTER_UP) ev.actionIndex else -1
        var sx = 0f
        var sy = 0f
        var n = 0
        for (i in 0 until ev.pointerCount) {
            if (i == skip) continue
            sx += ev.getX(i)
            sy += ev.getY(i)
            n++
        }
        if (n == 0) n = 1
        val cx = sx / n
        val cy = sy / n
        var span = 0f
        for (i in 0 until ev.pointerCount) {
            if (i == skip) continue
            span += hypot(ev.getX(i) - cx, ev.getY(i) - cy)
        }
        span /= n
        when (action) {
            MotionEvent.ACTION_DOWN -> {
                flingVx = 0f
                flingVy = 0f
                dragging = Native.dragStart(handle, ev.x, ev.y)
            }
            MotionEvent.ACTION_POINTER_DOWN -> if (dragging) {
                // A second finger turns a drag into pan/zoom.
                dragging = false
                if (Native.dragEnd(handle)) changed()
            }
            MotionEvent.ACTION_MOVE -> if (dragging) {
                Native.dragMove(handle, ev.x, ev.y)
                request()
            } else if (n == lastCount) {
                Native.pan(handle, cx - lastCx, cy - lastCy)
                if (n >= 2 && lastSpan > 1f && span > 1f) Native.zoom(handle, span / lastSpan, cx, cy)
                request()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (dragging) {
                    dragging = false
                    if (Native.dragEnd(handle)) changed() else request()
                } else if (action == MotionEvent.ACTION_UP && lastCount == 1) {
                    vt.computeCurrentVelocity(1000)
                    val vx = vt.xVelocity
                    val vy = vt.yVelocity
                    if (hypot(vx, vy) > 400f) {
                        flingVx = vx
                        flingVy = vy
                        lastFrameNs = 0L
                        request()
                    }
                }
                vt.recycle()
                velocity = null
                n = 0
            }
        }
        lastCx = cx
        lastCy = cy
        lastSpan = span
        lastCount = n
    }

    companion object {
        const val REQ_OPEN = 1
        const val REQ_NEW = 2
        const val EMPTY = "{\"hyperschema\":1,\"nodes\":[],\"edges\":[],\"dimensions\":[{\"id\":\"main\",\"name\":\"Основной\",\"nest\":\"contains\"}]}"
    }
}
