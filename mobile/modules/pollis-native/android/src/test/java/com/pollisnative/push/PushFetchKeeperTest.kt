package com.pollisnative.push

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** #1804: the push fetch's foreground service is started once, stopped once, and never too early. */
class PushFetchKeeperTest {
  private class FakeService : PushFetchKeeper.Foreground {
    var entered = 0
    var left = 0
    override fun enter(): Boolean {
      entered += 1
      return true
    }
    override fun leave() {
      left += 1
    }
  }

  private var starts = 0
  private val start = { starts += 1; true }

  @Before
  fun reset() {
    PushFetchKeeper.resetForTest()
    starts = 0
  }

  @After
  fun clean() {
    PushFetchKeeper.resetForTest()
  }

  @Test
  fun overlappingPushesStartOneServiceAndTheLastReleaseStopsIt() {
    assertTrue(PushFetchKeeper.acquire(start))
    assertTrue(PushFetchKeeper.acquire(start))
    assertEquals("one start for two holds", 1, starts)
    val service = FakeService()
    PushFetchKeeper.onStarted(service)
    assertEquals(1, service.entered)

    PushFetchKeeper.release()
    assertEquals("still held by the second push", 0, service.left)
    PushFetchKeeper.release()
    assertEquals(1, service.left)
    assertEquals(0, PushFetchKeeper.holdsForTest())
  }

  /** Stopping before `startForeground` would crash the app: the service enters, then leaves. */
  @Test
  fun aReleaseBeforeTheServiceStartsStillEntersTheForegroundFirst() {
    assertTrue(PushFetchKeeper.acquire(start))
    PushFetchKeeper.release()
    val service = FakeService()
    PushFetchKeeper.onStarted(service)
    assertEquals("startForeground is always called", 1, service.entered)
    assertEquals("and the unheld service stops itself", 1, service.left)
  }

  @Test
  fun aRefusedStartHoldsNothing() {
    assertFalse(PushFetchKeeper.acquire { false })
    assertEquals(0, PushFetchKeeper.holdsForTest())
    // The next push tries again.
    assertTrue(PushFetchKeeper.acquire(start))
    assertEquals(1, starts)
  }

  @Test
  fun aPushAfterTheServiceStoppedStartsItAgain() {
    assertTrue(PushFetchKeeper.acquire(start))
    val first = FakeService()
    PushFetchKeeper.onStarted(first)
    PushFetchKeeper.release()
    assertEquals(1, first.left)

    assertTrue(PushFetchKeeper.acquire(start))
    assertEquals(2, starts)
  }

  @Test
  fun anExtraReleaseNeverGoesNegative() {
    PushFetchKeeper.release()
    assertEquals(0, PushFetchKeeper.holdsForTest())
  }
}
