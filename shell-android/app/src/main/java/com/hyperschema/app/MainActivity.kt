package com.hyperschema.app

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.OpenableColumns
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.util.TypedValue
import android.view.Choreographer
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.TextView
import kotlin.math.max
import kotlin.math.min

/**
 * Thin shell: the Rust core (shared editor + finger gestures) does the
 * work; this draws a few controls over the canvas and keeps the file.
 *
 * No dialogs: text is typed straight into the concept (a hidden field feeds
 * the keyboard, the engine draws text and caret); a selection or a tapped
 * connection gets a small bar next to it. Gestures are described in
 * `core/src/touch.rs`. Changes save themselves half a second later.
 */
class MainActivity : Activity(), SurfaceHolder.Callback, Choreographer.FrameCallback {
    private var handle = 0L
    private var creating = false
    private var attached = false
    private lateinit var surfaceView: SurfaceView
    private lateinit var root: FrameLayout
    private lateinit var title: TextView
    private lateinit var undoBtn: TextView
    private lateinit var redoBtn: TextView
    private lateinit var plusBtn: TextView
    private lateinit var bar: LinearLayout
    private lateinit var field: Field
    private var scheduled = false
    private var dirty = false
    /** The file's text as we last read or wrote it: tells a sync's change from ours. */
    private var lastText: String? = null
    private var pendingText: String? = null
    private var uri: Uri? = null
    private val ui = Handler(Looper.getMainLooper())
    private val saver = Runnable { save() }
    private var topInset = 0
    private var bottomInset = 0
    private var imeBottom = 0
    private var editing = false
    private var syncingField = false
    private var barKind = ""
    /** Screen box of the selection (or of the concept being typed into). */
    private var selBox: List<Float>? = null

    private val ink = Color.rgb(30, 30, 30)
    private val canvasBg = Color.rgb(247, 246, 243)

    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, resources.displayMetrics)

    /** The keyboard's field: invisible, it only carries text to the core. */
    inner class Field(c: Context) : EditText(c) {
        override fun onKeyPreIme(keyCode: Int, event: KeyEvent): Boolean {
            if (keyCode == KeyEvent.KEYCODE_BACK && event.action == KeyEvent.ACTION_UP && editing) {
                call { Native.commitText(handle) }
                return true
            }
            return super.onKeyPreIme(keyCode, event)
        }
    }

    private fun round(text: String, size: Float, fill: Int, fg: Int, onClick: (View) -> Unit) = TextView(this).apply {
        this.text = text
        textSize = size
        setTextColor(fg)
        gravity = Gravity.CENTER
        isSoundEffectsEnabled = false
        background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(fill)
            setStroke(dp(1f).toInt(), Color.argb(28, 0, 0, 0))
        }
        elevation = dp(2f)
        setOnClickListener { onClick(it) }
    }

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        // Edge to edge, bars see-through over the canvas colour, dark icons.
        window.setDecorFitsSystemWindows(false)
        window.statusBarColor = Color.TRANSPARENT
        window.navigationBarColor = Color.TRANSPARENT
        window.isNavigationBarContrastEnforced = false
        window.isStatusBarContrastEnforced = false
        window.setBackgroundDrawable(ColorDrawable(canvasBg))
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)

        root = FrameLayout(this)
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        // Hidden until the first frame: no black flash while the GPU starts.
        surfaceView.alpha = 0f
        root.addView(surfaceView)

        val glass = Color.argb(235, 255, 255, 255)
        val b = dp(44f).toInt()
        val menu = round("☰", 18f, glass, ink) { showMenu(it) }
        title = TextView(this).apply {
            textSize = 14f
            setTextColor(Color.rgb(90, 90, 90))
            setPadding(dp(10f).toInt(), 0, 0, 0)
            maxLines = 1
        }
        val left = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(menu, LinearLayout.LayoutParams(b, b))
            addView(title)
        }
        undoBtn = round("↶", 20f, glass, ink) { call { Native.undo(handle, false) } }
        redoBtn = round("↷", 20f, glass, ink) { call { Native.undo(handle, true) } }
        val right = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            addView(undoBtn, LinearLayout.LayoutParams(b, b))
            addView(View(this@MainActivity), LinearLayout.LayoutParams(dp(8f).toInt(), 1))
            addView(redoBtn, LinearLayout.LayoutParams(b, b))
        }
        root.addView(left, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START))
        root.addView(right, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.END))
        plusBtn = round("+", 28f, ink, Color.WHITE) { call { Native.createCenter(handle) } }
        plusBtn.elevation = dp(4f)
        root.addView(plusBtn, FrameLayout.LayoutParams(dp(56f).toInt(), dp(56f).toInt(), Gravity.BOTTOM or Gravity.END))

        bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = GradientDrawable().apply {
                cornerRadius = dp(22f)
                setColor(Color.WHITE)
                setStroke(dp(1f).toInt(), Color.argb(24, 0, 0, 0))
            }
            elevation = dp(6f)
            visibility = View.GONE
            setPadding(dp(6f).toInt(), 0, dp(6f).toInt(), 0)
        }
        root.addView(bar, FrameLayout.LayoutParams(-2, dp(44f).toInt()))

        field = Field(this).apply {
            alpha = 0f
            isCursorVisible = false
            background = null
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(s: Editable?) {
                    if (syncingField || handle == 0L || !editing) return
                    Native.setText(handle, s.toString())
                    sync()
                }
            })
        }
        root.addView(field, FrameLayout.LayoutParams(dp(2f).toInt(), dp(2f).toInt(), Gravity.TOP or Gravity.START))

        root.setOnApplyWindowInsetsListener { _, insets ->
            val sys = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            topInset = sys.top
            bottomInset = sys.bottom
            imeBottom = insets.getInsets(WindowInsets.Type.ime()).bottom
            val m = dp(10f).toInt()
            (left.layoutParams as FrameLayout.LayoutParams).setMargins(m + sys.left, m + sys.top, 0, 0)
            (right.layoutParams as FrameLayout.LayoutParams).setMargins(0, m + sys.top, m + sys.right, 0)
            (plusBtn.layoutParams as FrameLayout.LayoutParams).setMargins(0, 0, dp(18f).toInt() + sys.right, dp(18f).toInt() + sys.bottom)
            left.requestLayout(); right.requestLayout(); plusBtn.requestLayout()
            keepTypingVisible()
            insets
        }
        setContentView(root)
        window.insetsController?.setSystemBarsAppearance(
            WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS,
            WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
        )

        surfaceView.setOnTouchListener { _, ev -> onTouch(ev); true }
        surfaceView.setOnGenericMotionListener { _, ev ->
            if (ev.actionMasked == MotionEvent.ACTION_SCROLL && handle != 0L) {
                Native.zoom(handle, Math.pow(2.0, ev.getAxisValue(MotionEvent.AXIS_VSCROLL) * 0.35).toFloat(), ev.x, ev.y)
                request()
                true
            } else false
        }

        // Debug: --es dbgfile NAME opens filesDir/NAME (emulator tests).
        val dbg = intent?.getStringExtra("dbgfile")
        if (dbg != null) open(Uri.fromFile(java.io.File(filesDir, dbg)))
        else if (!readIntent(intent)) {
            getPreferences(MODE_PRIVATE).getString("last", null)?.let { open(Uri.parse(it)) }
        }
        updateButtons()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val g = intent.getStringExtra("gest")
        if (g != null) {
            debugGesture(g, intent.getFloatExtra("x", 540f), intent.getFloatExtra("y", 1200f), intent.getFloatExtra("dx", 0f), intent.getFloatExtra("dy", 300f))
            return
        }
        intent.getStringExtra("type")?.let { t ->
            // Debug: types into the hidden field as the keyboard would.
            field.text.append(t)
            return
        }
        val dbg = intent.getStringExtra("dbgfile")
        if (dbg != null) open(Uri.fromFile(java.io.File(filesDir, dbg))) else readIntent(intent)
    }

    override fun onPause() {
        super.onPause()
        if (ui.hasCallbacks(saver)) {
            ui.removeCallbacks(saver)
            save()
        }
    }

    /** Back from another app (Obsidian syncing, say): pick up a changed file. */
    override fun onResume() {
        super.onResume()
        val u = uri ?: return
        if (handle == 0L || ui.hasCallbacks(saver) || !Native.editable(handle)) return
        val text = readText(u) ?: return
        if (text == lastText) return
        if (Native.reload(handle, text)) {
            lastText = text
            sync()
        }
    }

    private fun readText(u: Uri): String? =
        runCatching { contentResolver.openInputStream(u)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()

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
        return false
    }

    private fun open(u: Uri) {
        val text = readText(u)
        if (text == null) {
            title.text = "не открылось"
            return
        }
        uri = u
        lastText = text
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
        if (err != null) title.text = "не открылось: $err"
        else if (!Native.editable(handle)) title.text = "${title.text} · только просмотр"
        sync()
    }

    private fun save() {
        val u = uri ?: return
        if (handle == 0L || !Native.editable(handle)) return
        val text = Native.serialize(handle)
        // Someone else's version (a sync) is about to be overwritten: keep a copy.
        val disk = readText(u)
        if (disk != null && lastText != null && disk != lastText) {
            runCatching {
                val dir = java.io.File(filesDir, "backups").apply { mkdirs() }
                java.io.File(dir, "${displayName(u)} ${System.currentTimeMillis()}.hyperschema").writeText(disk)
            }
        }
        val ok = runCatching { contentResolver.openOutputStream(u, "wt")?.use { it.write(text.toByteArray(Charsets.UTF_8)) } != null }.getOrDefault(false)
        if (ok) lastText = text
        if (!ok) title.text = "${displayName(u)} · не сохраняется, откройте через ☰"
    }

    private fun showMenu(anchor: View) {
        val m = PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Открыть…")
        m.menu.add(0, 2, 1, "Новая схема…")
        m.menu.add(0, 3, 2, "Вписать всё")
        m.setOnMenuItemClickListener {
            when (it.itemId) {
                1 -> startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"), REQ_OPEN)
                2 -> startActivityForResult(
                    Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("application/octet-stream")
                        .putExtra(Intent.EXTRA_TITLE, "Новая схема.hyperschema"),
                    REQ_NEW
                )
                3 -> call { Native.fit(handle) }
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

    /** Runs a core call, then brings the controls in line with the core. */
    private fun call(f: () -> Unit) {
        if (handle == 0L) return
        f()
        sync()
    }

    /** Reads what the core wants shown: text field, bars, save. */
    private fun sync() {
        if (handle == 0L) return
        var text: String? = null
        var menu: List<String>? = null
        selBox = null
        for (line in Native.poll(handle).split('\n')) {
            val p = line.split('\t')
            when (p[0]) {
                "dirty" -> {
                    ui.removeCallbacks(saver)
                    ui.postDelayed(saver, 500)
                }
                "held" -> surfaceView.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                "text" -> text = line.substringAfter('\t').replace('\u0001', '\n')
                "sel" -> selBox = p.drop(1).map { it.toFloat() }
                "menu" -> menu = p.drop(1)
            }
        }
        val imm = getSystemService(InputMethodManager::class.java)
        if (text != null && !editing) {
            editing = true
            syncingField = true
            field.setText(text)
            field.setSelection(field.text.length)
            syncingField = false
            field.requestFocus()
            imm.showSoftInput(field, 0)
        } else if (text == null && editing) {
            editing = false
            imm.hideSoftInputFromWindow(field.windowToken, 0)
            field.clearFocus()
        }
        val sel = selBox
        when {
            menu != null -> showEdgeBar(menu)
            sel != null && !editing -> showSelBar(sel)
            else -> hideBar()
        }
        if (editing) keepTypingVisible()
        plusBtn.visibility = if (editing || !Native.editable(handle)) View.GONE else View.VISIBLE
        updateButtons()
        request()
    }

    /** The concept being typed into stays above the keyboard. */
    private fun keepTypingVisible() {
        val s = selBox ?: return
        if (!editing || handle == 0L) return
        val limit = root.height - max(imeBottom, bottomInset) - dp(24f)
        if (s[3] > limit) {
            Native.pan(handle, 0f, limit - s[3])
            request()
        }
    }

    private fun updateButtons() {
        val on = handle != 0L
        undoBtn.alpha = if (on && Native.canUndo(handle, false)) 1f else 0.35f
        redoBtn.alpha = if (on && Native.canUndo(handle, true)) 1f else 0.35f
    }

    private fun barButton(text: String, on: Boolean, f: () -> Unit) = TextView(this).apply {
        this.text = text
        textSize = 15f
        setTextColor(if (on) Color.rgb(0x2f, 0x5f, 0xe0) else ink)
        gravity = Gravity.CENTER
        isSoundEffectsEnabled = false
        setPadding(dp(12f).toInt(), 0, dp(12f).toInt(), 0)
        setOnClickListener { call(f) }
    }

    private fun placeBar(cx: Float, top: Float, bottom: Float) {
        bar.visibility = View.VISIBLE
        bar.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED)
        val w = bar.measuredWidth.toFloat()
        val h = dp(44f)
        // Clear of the connection dots around the selection.
        val gap = dp(46f)
        val above = top - gap - h
        val y = if (above > topInset + dp(64f)) above else min(bottom + gap, root.height - h - max(bottomInset, imeBottom) - gap)
        bar.x = (cx - w / 2).coerceIn(dp(8f), max(dp(8f), root.width - w - dp(8f)))
        bar.y = y
    }

    private fun showSelBar(s: List<Float>) {
        if (barKind != "sel") {
            bar.removeAllViews()
            bar.addView(barButton("Текст", false) { Native.editSelected(handle) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Удалить", false) { Native.deleteSelection(handle) }, LinearLayout.LayoutParams(-2, -1))
            barKind = "sel"
        }
        placeBar((s[0] + s[2]) / 2, s[1], s[3])
    }

    private fun showEdgeBar(m: List<String>) {
        val from = m[2] == "1"
        val to = m[3] == "1"
        val kind = "edge$from$to"
        if (barKind != kind) {
            bar.removeAllViews()
            bar.addView(barButton("—", !from && !to) { Native.edgeChoice(handle, 1) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("→", !from && to) { Native.edgeChoice(handle, 2) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("←", from && !to) { Native.edgeChoice(handle, 3) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("↔", from && to) { Native.edgeChoice(handle, 4) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Подпись", false) { Native.edgeChoice(handle, 5) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Удалить", false) { Native.edgeChoice(handle, 6) }, LinearLayout.LayoutParams(-2, -1))
            barKind = kind
        }
        val x = m[0].toFloat()
        val y = m[1].toFloat()
        placeBar(x, y - dp(10f), y + dp(10f))
    }

    private fun hideBar() {
        bar.visibility = View.GONE
        barKind = ""
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
                        sync()
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
        val busy = Native.tick(handle, SystemClock.uptimeMillis())
        if (busy) sync()
        if (dirty || busy) {
            dirty = false
            if (Native.render(handle) && surfaceView.alpha < 1f) surfaceView.alpha = 1f
        }
        if (busy) request()
    }

    private fun onTouch(ev: MotionEvent) {
        if (handle == 0L) return
        val t = ev.eventTime
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val i = ev.actionIndex
                Native.touch(handle, 0, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until ev.pointerCount) Native.touch(handle, 1, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val i = ev.actionIndex
                Native.touch(handle, 2, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_CANCEL -> Native.touch(handle, 3, 0, 0f, 0f, t)
        }
        sync()
    }

    /** Debug (adb): a synthetic gesture with exact timing — tap, dbl, qs, drag, long, ldrag, pinch. */
    private fun debugGesture(name: String, x: Float, y: Float, dx: Float, dy: Float) {
        val t0 = SystemClock.uptimeMillis() + 50
        fun ev(dt: Long, a: Int, px: Float, py: Float, id: Long = 0) {
            ui.postAtTime({ if (handle != 0L) { Native.touch(handle, a, id, px, py, t0 + dt); sync() } }, t0 + dt)
        }
        fun tap(at: Long) { ev(at, 0, x, y); ev(at + 60, 2, x, y) }
        fun line(at: Long) { for (i in 1..20) ev(at + i * 16L, 1, x + dx * i / 20, y + dy * i / 20); ev(at + 21 * 16L + 60, 2, x + dx, y + dy) }
        when (name) {
            "tap" -> tap(0)
            "dbl" -> { tap(0); tap(180) }
            "qs" -> { tap(0); ev(180, 0, x, y); line(180) }
            "drag" -> { ev(0, 0, x, y); line(0) }
            "long" -> { ev(0, 0, x, y); ev(700, 2, x, y) }
            "ldrag" -> { ev(0, 0, x, y); line(600) }
            "pinch" -> {
                ev(0, 0, x - 150, y, 0); ev(10, 0, x + 150, y, 1)
                for (i in 1..30) { val r = 150f * (1 + i / 30f); ev(10 + i * 16L, 1, x - r, y, 0); ev(10 + i * 16L, 1, x + r, y, 1) }
                ev(600, 2, x - 300, y, 0); ev(640, 2, x + 300, y, 1)
            }
        }
    }

    companion object {
        const val REQ_OPEN = 1
        const val REQ_NEW = 2
        const val EMPTY = "{\"hyperschema\":1,\"nodes\":[],\"edges\":[],\"dimensions\":[{\"id\":\"main\",\"name\":\"Основной\",\"nest\":\"contains\"}]}"
    }
}
