package local.kvm.mobile

import org.junit.Assert.*
import org.junit.Test

class TrackpadTest {
    @Test fun singleTapNeverClicksAndDoubleTapClicksOnce() {
        var clicks = 0
        val pad = Trackpad({ _, _ -> }, { clicks++ })
        pad.down(10.0, 10.0, 0); pad.up(10.0, 10.0, 50)
        assertEquals(0, clicks)
        pad.down(12.0, 11.0, 120); pad.up(12.0, 11.0, 180)
        assertEquals(1, clicks)
        pad.down(12.0, 11.0, 220); pad.up(12.0, 11.0, 260)
        assertEquals(1, clicks)
    }

    @Test fun draggingPreservesRelativeMotionAndCancelsTap() {
        var clicks = 0
        val motion = mutableListOf<Pair<Double, Double>>()
        val pad = Trackpad({ x, y -> motion.add(x to y) }, { clicks++ })
        pad.down(100.0, 80.0, 0); pad.move(120.0, 75.0); pad.move(118.0, 90.0); pad.up(118.0, 90.0, 100)
        pad.down(118.0, 90.0, 150); pad.up(118.0, 90.0, 200)
        assertEquals(listOf(20.0 to -5.0, -2.0 to 15.0), motion)
        assertEquals(0, clicks)
    }

    @Test fun longPressDistantTapAndSlowTapDoNotClick() {
        var clicks = 0
        val pad = Trackpad({ _, _ -> }, { clicks++ })
        pad.down(0.0, 0.0, 0); pad.up(0.0, 0.0, 300)
        pad.down(0.0, 0.0, 350); pad.up(0.0, 0.0, 400)
        pad.down(50.0, 0.0, 450); pad.up(50.0, 0.0, 500)
        pad.down(50.0, 0.0, 1000); pad.up(50.0, 0.0, 1050)
        assertEquals(0, clicks)
    }

    @Test fun multiTouchAndCancellationCannotProduceClick() {
        var clicks = 0; var motions = 0
        val pad = Trackpad({ _, _ -> motions++ }, { clicks++ })
        pad.down(0.0, 0.0, 0); pad.up(0.0, 0.0, 50)
        pad.down(0.0, 0.0, 100); pad.block(); pad.move(20.0, 0.0); pad.up(0.0, 0.0, 150)
        pad.down(0.0, 0.0, 200); pad.cancel(); pad.up(0.0, 0.0, 250)
        assertEquals(0, clicks); assertEquals(0, motions)
    }

    @Test fun keyboardMapsUsLettersAndPunctuationAndRejectsUnsupportedCharacters() {
        assertEquals(4 to 0, HidKeyboard.character('a'))
        assertEquals(4 to 2, HidKeyboard.character('A'))
        assertEquals(32 to 2, HidKeyboard.character('#'))
        assertEquals(40 to 0, HidKeyboard.character('\n'))
        assertEquals(44 to 0, HidKeyboard.character(' '))
        assertEquals(52 to 2, HidKeyboard.character('"'))
        assertNull(HidKeyboard.character('ñ'))
    }
}
