package io.github.drybearr.elsewhere

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
        waitFor("stopped") { it.entries.isEmpty() }
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
