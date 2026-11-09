package io.github.drybearr.elsewhere

import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.printToString
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performSemanticsAction
import androidx.lifecycle.Lifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.drybearr.elsewhere.playback.Playback
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.elsewhere.CollectionSummary
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.MediaFormat
import uniffi.elsewhere.PlayerStatus
import uniffi.elsewhere.PlaylistOptions
import uniffi.elsewhere.QueueState
import uniffi.elsewhere.RepeatMode
import uniffi.elsewhere.categories
import uniffi.elsewhere.collectionItems
import uniffi.elsewhere.collections
import uniffi.elsewhere.item
import uniffi.elsewhere.itemFile
import uniffi.elsewhere.job
import uniffi.elsewhere.savePlaylist

/**
 * The Music tab on the real app, end to end: a playlist page on the CI machine
 * (scripts/selftest_server.py) is saved through yt-dlp like the Add tab does, then played
 * from the Music tab and the player bar.
 *
 *   gradle -p android connectedDebugAndroidTest \
 *     -Pandroid.testInstrumentationRunnerArguments.server=http://10.0.2.2:8765
 */
@RunWith(AndroidJUnit4::class)
class MusicTest {
    @get:Rule
    val ui = createAndroidComposeRule<MainActivity>()

    private val server = InstrumentationRegistry.getArguments().getString("server") ?: "http://10.0.2.2:8765"
    private var removed: File? = null

    @After
    fun putBack() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync { Playback.stop() }
        // The song file the test took away comes back for the next run (the debug build
        // carries tests/media).
        removed?.let { file ->
            instrumentation.targetContext.assets.open("song.m4a").use { input ->
                file.outputStream().use { input.copyTo(it) }
            }
        }
    }

    @Test
    fun aSavedPlaylistPlaysFromTheMusicTab() {
        val list = save()
        val titles = collectionItems(list.id, null, 0u).entries.map { it.item.title }
        assertEquals(listOf("long", "song"), titles)

        openPlaylist()
        ui.onNodeWithText("2 songs").assertExists()
        // Play from the top: the bar shows the first song, with a next one.
        ui.onNode(hasText("Play") and hasClickAction()).performClick()
        waitFor("long playing") { it.nowPlaying?.title == "long" && it.playback.status == PlayerStatus.PLAYING }
        ui.waitUntil(10_000) { tagText("now-title") == "long" }
        ui.onNodeWithContentDescription("Next").assertIsEnabled()

        // Pause and play again.
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("paused") { !it.wantsPlay && it.playback.status == PlayerStatus.PAUSED }
        ui.onNodeWithContentDescription("Play").performClick()
        waitFor("playing again") { it.wantsPlay && it.playback.status == PlayerStatus.PLAYING }

        // Next: the last song, with no next after it; previous at its start goes back.
        ui.onNodeWithContentDescription("Next").performClick()
        waitFor("song") { it.nowPlaying?.title == "song" }
        ui.waitUntil(10_000) { tagText("now-title") == "song" }
        ui.onNodeWithContentDescription("Next").assertIsNotEnabled()
        ui.onNodeWithContentDescription("Previous").performClick()
        waitFor("long again") { it.nowPlaying?.title == "long" }

        // To the end: "Finished", and Play starts over from the top.
        waitFor("the end", 30_000) { it.finished }
        ui.waitUntil(10_000) { tagText("now-below") == "Finished" }
        ui.onNodeWithContentDescription("Play").performClick()
        waitFor("from the top") { it.current == 0u && it.wantsPlay }

        // Tapping a song plays from it.
        ui.onNodeWithTag("song-1").performClick()
        waitFor("song from its row") { it.nowPlaying?.title == "song" && it.wantsPlay }

        // Stop: the bar goes away.
        ui.onNodeWithContentDescription("Stop").performClick()
        waitFor("stopped") { it.entryCount == 0u }
        ui.waitUntil(10_000) { ui.onAllNodes(hasContentDescription("Stop")).fetchSemanticsNodes().isEmpty() }

        // Plays were counted, and the library learned the lengths from playing.
        val first = collectionItems(list.id, null, 0u).entries.first().item
        assertTrue("long was played", (item(first.id).summary.progress?.plays ?: 0) >= 1)
        assertEquals(6L, ((first.durationMs ?: 0) + 500) / 1000)

        // A song whose file is gone: skipped, and its row says so.
        val second = collectionItems(list.id, null, 0u).entries[1].item
        val file = File(itemFile(second.id))
        removed = file
        assertTrue(file.delete())
        ui.onNodeWithTag("song-0").performClick()
        waitFor("the end without song", 30_000) { it.finished && it.skipped.any { skip -> skip.itemId == second.id } }
        ui.waitUntil(10_000) { tagText("note-1") == "Could not play" }

        // Back to the list of playlists.
        ui.onNodeWithContentDescription("Back").performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasText("Music Playlist") and hasClickAction()).fetchSemanticsNodes().isNotEmpty() }
    }

    @Test
    fun thePlayerScreenControlsPlaybackAndTheQueue() {
        save()
        openPlaylist()
        ui.onNode(hasText("Play") and hasClickAction()).performClick()
        waitFor("long playing") { it.nowPlaying?.title == "long" && it.playback.status == PlayerStatus.PLAYING }
        // Paused, so long stays on while the screens are checked.
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("paused") { !it.wantsPlay }

        // Open it from the bar, close it, open it again.
        ui.onNodeWithTag("open-player").performClick()
        ui.waitUntil(10_000) { tagText("player-title") == "long" }
        ui.onNodeWithContentDescription("Close player").performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("open-player")).fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("open-player").performClick()
        ui.waitUntil(10_000) { tagText("player-title") == "long" }

        // Repeat goes round off, all, one; shuffle on and off; speed from the menu.
        ui.onNodeWithTag("repeat").performClick()
        waitFor("repeat all") { it.repeat == RepeatMode.ALL }
        ui.onNodeWithContentDescription("Repeat all").assertExists()
        ui.onNodeWithTag("repeat").performClick()
        waitFor("repeat one") { it.repeat == RepeatMode.ONE }
        ui.onNodeWithTag("repeat").performClick()
        waitFor("repeat off") { it.repeat == RepeatMode.OFF }
        ui.onNodeWithTag("shuffle").performClick()
        waitFor("shuffled") { it.shuffle }
        ui.onNodeWithTag("shuffle").performClick()
        waitFor("not shuffled") { !it.shuffle }
        ui.onNodeWithTag("speed").performClick()
        ui.onNodeWithText("2x").performClick()
        waitFor("speed 2") { it.playback.speed == 2.0 }
        ui.onNodeWithTag("speed").performClick()
        ui.onNodeWithText("1x").performClick()
        waitFor("speed 1") { it.playback.speed == 1.0 }

        // Paused, the slider goes to 3 s and the player follows.
        waitFor("paused") { it.playback.status == PlayerStatus.PAUSED }
        waitFor("a length") { (it.playback.durationMs ?: 0) > 0 }
        ui.onNodeWithTag("player-position").performSemanticsAction(SemanticsActions.SetProgress) { it(3000f) }
        waitFor("at 3 s") { it.playback.positionMs in 2500..3600 }
        ui.waitUntil(10_000) { tagText("player-elapsed") in setOf("0:02", "0:03") }

        // The queue, below the player: rows with their titles; tap one to play it.
        ui.waitUntil(15_000) { ui.onAllNodes(hasTestTag("player-list")).fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("player-list").performScrollToNode(hasTestTag("queue-1"))
        waitRows("song second") { shownRowTitle("queue-1") == "song" }
        ui.onNodeWithTag("queue-1").performClick()
        waitFor("song from the queue") { it.nowPlaying?.title == "song" && it.wantsPlay }
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("paused on song") { !it.wantsPlay }
        // Play next brings long after song; remove takes it out again.
        menu("queue-menu-0", "Play next")
        waitFor("long next") { it.hasNext && it.current == 0u }
        waitRows("long second") { shownRowTitle("queue-1") == "long" }
        menu("queue-menu-1", "Remove")
        waitFor("one left") { it.entryCount == 1u && !it.hasNext }
        // Removing the last one empties the queue: the player closes itself.
        menu("queue-menu-0", "Remove")
        waitFor("empty") { it.entryCount == 0u }
        ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("player-title")).fetchSemanticsNodes().isEmpty() }

        // From a playlist: add to an empty queue plays; play next comes after it.
        menu("song-menu-1", "Add to queue")
        waitFor("song added and playing") { it.entryCount == 1u && it.nowPlaying?.title == "song" && it.wantsPlay }
        // Paused, so the song is still there when the menu is done.
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("song paused") { !it.wantsPlay && !it.finished }
        menu("song-menu-0", "Play next")
        waitFor("long after song") { it.entryCount == 2u && it.hasNext }
        ui.onNodeWithContentDescription("Next").performClick()
        waitFor("long next", 15_000) { it.nowPlaying?.title == "long" }
    }

    @Test
    fun aVideoShowsInThePlayerAndPlaysOnWithoutIt() {
        val list = save("video-playlist.html", "Videos", "Video Playlist", MediaFormat.Video(null))
        assertEquals(listOf("long", "clip"), collectionItems(list.id, null, 0u).entries.map { it.item.title })
        ui.onNode(hasText("Videos") and hasClickAction()).performClick()
        ui.waitUntil(20_000) { ui.onAllNodes(hasText("Video Playlist") and hasClickAction()).fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(hasText("Video Playlist") and hasClickAction()).performClick()
        ui.waitUntil(20_000) { ui.onAllNodes(hasText("2 videos")).fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(hasText("Play") and hasClickAction()).performClick()
        // Without a screen for it, a video plays its sound only.
        waitFor("long playing") { it.nowPlaying?.title == "long" && it.playback.status == PlayerStatus.PLAYING }
        waitFor("its shape") { it.playback.hasVideo && it.playback.videoWidth == 96u && it.playback.videoHeight == 64u }
        assertTrue("no picture without a screen", Playback.state.value?.playback?.videoShown == false)

        // The player shows the picture; closing it turns the picture off, the sound plays on.
        ui.onNodeWithTag("open-player").performClick()
        waitFor("the picture shown", 20_000) { it.playback.videoShown }
        ui.onNodeWithContentDescription("Close player").performClick()
        waitFor("the picture off") { !it.playback.videoShown }
        val at = Playback.state.value!!.playback.positionMs
        waitFor("the sound playing on") { it.wantsPlay && it.playback.positionMs > at + 500 }

        // Full screen and back.
        ui.onNodeWithTag("open-player").performClick()
        waitFor("the picture again", 20_000) { it.playback.videoShown }
        ui.onNodeWithContentDescription("Full screen").performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("video-full")).fetchSemanticsNodes().isNotEmpty() }
        waitFor("the picture full screen", 20_000) { it.playback.videoShown }
        ui.onNodeWithTag("video-full").performClick()
        ui.onNodeWithContentDescription("Leave full screen").performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("video-full")).fetchSemanticsNodes().isEmpty() }
        waitFor("the picture back in the player", 20_000) { it.playback.videoShown }

        // The app in the background: no picture, the sound plays on; back: the picture again.
        ui.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        waitFor("no picture in the background") { !it.playback.videoShown }
        waitFor("still playing") { it.wantsPlay }
        ui.activityRule.scenario.moveToState(Lifecycle.State.RESUMED)
        waitFor("the picture after coming back", 20_000) { it.playback.videoShown }
    }

    @Test
    fun theMusicTabSaysWhenThereIsNothing() {
        // Whatever is saved, the tab opens; an empty Music tab says how to fill it.
        ui.onNode(hasText("Music") and hasClickAction()).performClick()
        ui.waitUntil(20_000) {
            ui.onAllNodes(hasText("Music Playlist")).fetchSemanticsNodes().isNotEmpty() ||
                ui.onAllNodes(hasText("No playlists yet. Save one from the Add tab.")).fetchSemanticsNodes().isNotEmpty()
        }
    }

    /** Saves a test playlist (songs by default), the way the Add tab does; again is fine. */
    private fun save(
        page: String = "music-playlist.html",
        tab: String = "Music",
        name: String = "Music Playlist",
        format: MediaFormat = MediaFormat.Audio,
    ): CollectionSummary {
        val id = savePlaylist("$server/$page", PlaylistOptions(format, false, null))
        val deadline = System.currentTimeMillis() + 180_000
        while (true) {
            val state = job(id)
            if (state != null && state.status.isFinished) {
                assertEquals("$state", JobStatus.DONE, state.status)
                break
            }
            assertTrue("the save did not finish: $state", System.currentTimeMillis() < deadline)
            Thread.sleep(100)
        }
        val found = categories().first { it.name == tab }
        return collections(found.id).first { it.name == name }
    }

    private fun openPlaylist() {
        ui.onNode(hasText("Music") and hasClickAction()).performClick()
        ui.waitUntil(20_000) { ui.onAllNodes(hasText("Music Playlist") and hasClickAction()).fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(hasText("Music Playlist") and hasClickAction()).performClick()
        ui.waitUntil(20_000) { ui.onAllNodes(hasTestTag("song-1")).fetchSemanticsNodes().isNotEmpty() }
    }

    /**
     * The text of the node with this tag, or null when there is none. The unmerged tree:
     * a tagged text inside a clickable row is merged into the row otherwise.
     */
    private fun tagText(tag: String): String? =
        ui.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().firstOrNull()
            ?.config?.getOrNull(SemanticsProperties.Text)
            ?.joinToString("") { it.text }

    /** A queue row's title, scrolled into view first (it may be below the player). */
    private fun shownRowTitle(tag: String): String? {
        runCatching { ui.onNodeWithTag("player-list").performScrollToNode(hasTestTag(tag)) }
        return rowTitle(tag)
    }

    /** The first text inside the node with this tag (a row's title). */
    private fun rowTitle(tag: String): String? =
        ui.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().firstOrNull()
            ?.children?.firstNotNullOfOrNull { child ->
                child.config.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }
                    ?: child.children.firstNotNullOfOrNull { inner ->
                        inner.config.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }
                    }
            }

    /** The end of the core's own log (mpv's errors go there): the app is gone after the run. */
    private fun coreLog(): String = runCatching {
        File(InstrumentationRegistry.getInstrumentation().targetContext.filesDir, "logs/elsewhere.log")
            .readLines().takeLast(40).joinToString("\n")
    }.getOrElse { "(none: $it)" }

    /** Opens the menu with this tag and picks `item`; waits for the menu to close again. */
    private fun menu(tag: String, item: String) {
        // A queue row may be below the player: into view first.
        if (tag.startsWith("queue-")) ui.onNodeWithTag("player-list").performScrollToNode(hasTestTag(tag))
        ui.onNodeWithTag(tag).performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasText(item)).fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithText(item).performClick()
        ui.waitUntil(10_000) { ui.onAllNodes(hasText(item)).fetchSemanticsNodes().isEmpty() }
        ui.waitForIdle()
    }

    /** What the screen shows (its semantics tree), for failures. */
    private fun screen(): String = runCatching { ui.onRoot().printToString(maxDepth = 30) }.getOrElse { "(no tree: $it)" }

    /** Waits for the queue's rows to hold; if they never do, says what they show. */
    private fun waitRows(what: String, check: () -> Boolean) {
        try {
            ui.waitUntil(15_000, check)
        } catch (e: androidx.compose.ui.test.ComposeTimeoutException) {
            val rows = (0..3).map { "queue-$it=${rowTitle("queue-$it")}" }
            throw AssertionError(
                "never $what: rows $rows, entries ${Playback.entries.value}, state ${Playback.state.value}\nscreen:\n${screen()}",
                e,
            )
        }
    }

    /** Waits for the queue's state to hold. */
    private fun waitFor(what: String, timeout: Long = 15_000, check: (QueueState) -> Boolean) {
        val deadline = System.currentTimeMillis() + timeout
        while (true) {
            val state = Playback.state.value
            if (state != null && check(state)) return
            if (System.currentTimeMillis() >= deadline) {
                throw AssertionError(
                    "never $what: $state\nvideo: ${Playback.videoEvents.toList()}\ncore log:\n${coreLog()}\nscreen:\n${screen()}",
                )
            }
            Thread.sleep(20)
        }
    }
}
