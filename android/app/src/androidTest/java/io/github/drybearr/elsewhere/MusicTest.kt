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
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performSemanticsAction
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
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("paused") { it.playback.status == PlayerStatus.PAUSED }
        waitFor("a length") { (it.playback.durationMs ?: 0) > 0 }
        ui.onNodeWithTag("player-position").performSemanticsAction(SemanticsActions.SetProgress) { it(3000f) }
        waitFor("at 3 s") { it.playback.positionMs in 2500..3600 }
        ui.waitUntil(10_000) { tagText("player-elapsed") in setOf("0:02", "0:03") }

        // The queue, below the player: rows with their titles; tap one to play it.
        ui.waitUntil(15_000) { ui.onAllNodes(hasTestTag("player-list")).fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("player-list").performScrollToNode(hasTestTag("queue-1"))
        ui.waitUntil(15_000) { rowTitle("queue-1") == "song" }
        ui.onNodeWithTag("queue-1").performClick()
        waitFor("song from the queue") { it.nowPlaying?.title == "song" && it.wantsPlay }
        ui.onNodeWithContentDescription("Pause").performClick()
        waitFor("paused on song") { !it.wantsPlay }
        // Play next brings long after song; remove takes it out again.
        ui.onNodeWithTag("queue-menu-0").performClick()
        ui.onNodeWithText("Play next").performClick()
        waitFor("long next") { it.hasNext && it.current == 0u }
        ui.waitUntil(15_000) { rowTitle("queue-1") == "long" }
        ui.onNodeWithTag("queue-menu-1").performClick()
        ui.onNodeWithText("Remove").performClick()
        waitFor("one left") { it.entryCount == 1u && !it.hasNext }
        // Removing the last one empties the queue: the player closes itself.
        ui.onNodeWithTag("queue-menu-0").performClick()
        ui.onNodeWithText("Remove").performClick()
        waitFor("empty") { it.entryCount == 0u }
        ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("player-title")).fetchSemanticsNodes().isEmpty() }

        // From a playlist: add to an empty queue plays; play next comes after it.
        ui.onNodeWithTag("song-menu-1").performClick()
        ui.onNodeWithText("Add to queue").performClick()
        waitFor("song added and playing") { it.entryCount == 1u && it.nowPlaying?.title == "song" && it.wantsPlay }
        ui.onNodeWithTag("song-menu-0").performClick()
        ui.onNodeWithText("Play next").performClick()
        waitFor("long after song") { it.entryCount == 2u && it.hasNext }
        waitFor("long plays next", 15_000) { it.nowPlaying?.title == "long" }
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

    /** Saves the test playlist as songs, the way the Add tab does; again is fine. */
    private fun save(): CollectionSummary {
        val id = savePlaylist("$server/music-playlist.html", PlaylistOptions(MediaFormat.Audio, false, null))
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
        val music = categories().first { it.name == "Music" }
        return collections(music.id).first { it.name == "Music Playlist" }
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

    /** The first text inside the node with this tag (a row's title). */
    private fun rowTitle(tag: String): String? =
        ui.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().firstOrNull()
            ?.children?.firstNotNullOfOrNull { child ->
                child.config.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }
                    ?: child.children.firstNotNullOfOrNull { inner ->
                        inner.config.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }
                    }
            }

    /** Waits for the queue's state to hold. */
    private fun waitFor(what: String, timeout: Long = 15_000, check: (QueueState) -> Boolean) {
        val deadline = System.currentTimeMillis() + timeout
        while (true) {
            val state = Playback.state.value
            if (state != null && check(state)) return
            assertTrue("never $what: $state", System.currentTimeMillis() < deadline)
            Thread.sleep(20)
        }
    }
}
