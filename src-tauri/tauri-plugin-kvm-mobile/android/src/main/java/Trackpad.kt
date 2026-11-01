package local.kvm.mobile

import kotlin.math.hypot

/** Pure gesture state, with CSS-independent density-normalized coordinates. */
class Trackpad(private val motion: (Double, Double) -> Unit, private val click: () -> Unit) {
    private var active = false
    private var blocked = false
    private var x = 0.0; private var y = 0.0
    private var startX = 0.0; private var startY = 0.0; private var started = 0L
    private var moved = false
    private var lastTap: Long? = null
    private var tapX = 0.0; private var tapY = 0.0
    fun down(x: Double, y: Double, time: Long) {
        active = true; blocked = false; moved = false
        this.x = x; this.y = y; startX = x; startY = y; started = time
    }
    fun move(x: Double, y: Double) {
        if (!active || blocked) return
        if (hypot(x - startX, y - startY) > 8) { moved = true; lastTap = null }
        motion(x - this.x, y - this.y); this.x = x; this.y = y
    }
    fun up(x: Double, y: Double, time: Long) {
        val tap = active && !blocked && !moved && time - started <= 250 && hypot(x - startX, y - startY) <= 8
        active = false
        if (!tap) { lastTap = null; return }
        val previous = lastTap
        if (previous != null && time - previous <= 350 && hypot(x - tapX, y - tapY) <= 32) {
            lastTap = null; click()
        } else { lastTap = time; tapX = x; tapY = y }
    }
    fun block() { blocked = true; lastTap = null }
    fun cancel() { active = false; blocked = false; lastTap = null }
}
