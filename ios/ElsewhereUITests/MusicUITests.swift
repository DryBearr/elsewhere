import XCTest

/// The Music tab on the real app, end to end: the app saves a playlist page from this
/// machine (scripts/selftest_server.py) through yt-dlp like the Add tab does (`-selftest
/// music`), then this plays it from the Music tab and the player bar.
///
///   xcodebuild test -scheme Elsewhere -only-testing:ElsewhereUITests ...
final class MusicUITests: XCTestCase {
    private let server = ProcessInfo.processInfo.environment["ELSEWHERE_SERVER"] ?? "http://127.0.0.1:8765"

    override func setUp() {
        continueAfterFailure = false
    }

    func testASavedPlaylistPlaysFromTheMusicTab() {
        let app = XCUIApplication()
        app.launchArguments = ["-selftest", "music", "-server", server]
        app.launch()

        app.tabBars.buttons["Music"].tap()
        // Saving starts Python and yt-dlp first: give it time.
        let playlist = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Music Playlist'")).firstMatch
        wait(playlist, "exists == true", timeout: 180)
        XCTAssertTrue(playlist.label.contains("2 songs"), playlist.label)
        playlist.tap()
        let first = app.buttons["song-0"]
        let second = app.buttons["song-1"]
        wait(second, "exists == true", timeout: 30)
        XCTAssertTrue(first.label.hasPrefix("long"), first.label)
        XCTAssertTrue(second.label.hasPrefix("song"), second.label)

        // Play from the top: the bar shows the first song, with a next one.
        let title = app.staticTexts["now-title"]
        app.buttons["play-all"].tap()
        wait(title, "label == 'long'")
        XCTAssertTrue(app.buttons["bar-next"].isEnabled)

        // Pause and play again.
        app.buttons["bar-pause"].tap()
        wait(app.buttons["bar-play"], "exists == true")
        app.buttons["bar-play"].tap()
        wait(app.buttons["bar-pause"], "exists == true")

        // Next: the last song, with no next after it; previous at its start goes back.
        app.buttons["bar-next"].tap()
        wait(title, "label == 'song'")
        wait(app.buttons["bar-next"], "isEnabled == false")
        app.buttons["bar-previous"].tap()
        wait(title, "label == 'long'")

        // To the end: "Finished", and Play starts over from the top.
        wait(app.staticTexts["now-below"], "label == 'Finished'", timeout: 30)
        app.buttons["bar-play"].tap()
        wait(title, "label == 'long'")
        wait(app.buttons["bar-pause"], "exists == true")

        // Tapping a song plays from it.
        second.tap()
        wait(title, "label == 'song'")

        // Stop: the bar goes away.
        app.buttons["bar-stop"].tap()
        wait(app.buttons["bar-stop"], "exists == false")

        // A song whose file is gone: skipped, and its row says so.
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName("io.github.drybearr.elsewhere.selftest.remove-second-song" as CFString),
            nil, nil, true)
        sleep(2)
        first.tap()
        wait(second, "label CONTAINS 'Could not play'", timeout: 30)

        // Back to the list of playlists.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        wait(playlist, "exists == true")
    }

    func testThePlayerScreenControlsPlaybackAndTheQueue() {
        let app = XCUIApplication()
        app.launchArguments = ["-selftest", "music", "-server", server]
        app.launch()
        app.tabBars.buttons["Music"].tap()
        let playlist = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Music Playlist'")).firstMatch
        wait(playlist, "exists == true", timeout: 180)
        playlist.tap()
        wait(app.buttons["song-1"], "exists == true", timeout: 30)
        app.buttons["play-all"].tap()
        wait(app.staticTexts["now-title"], "label == 'long'")
        // Paused, so long stays on while the screens are checked (the simulator is slow).
        app.buttons["bar-pause"].tap()
        wait(app.buttons["bar-play"], "exists == true")

        // Open it from the bar, close it, open it again.
        let title = app.staticTexts["player-title"]
        let bar = app.otherElements["open-player"]
        wait(bar, "exists == true")
        bar.tap()
        wait(title, "label == 'long'", in: app)
        app.buttons["close-player"].tap()
        wait(title, "exists == false")
        bar.tap()
        wait(title, "label == 'long'", in: app)

        // Repeat goes round off, all, one; shuffle on and off; speed from the menu.
        let repeatButton = app.buttons["repeat"]
        repeatButton.tap()
        wait(repeatButton, "label == 'Repeat all'")
        repeatButton.tap()
        wait(repeatButton, "label == 'Repeat one'")
        repeatButton.tap()
        wait(repeatButton, "label == 'Repeat off'")
        app.buttons["shuffle"].tap()
        wait(app.buttons["shuffle"], "label == 'Shuffle on'")
        app.buttons["shuffle"].tap()
        wait(app.buttons["shuffle"], "label == 'Shuffle off'")
        app.buttons["speed"].tap()
        app.buttons["2x"].tap()
        wait(app.buttons["speed"], "label CONTAINS '2x'")
        // On at half speed: the 2 s song must not end while the queue is checked.
        app.buttons["speed"].tap()
        app.buttons["0.5x"].tap()
        wait(app.buttons["speed"], "label CONTAINS '0.5x'")

        // Paused, the slider goes to the middle (3 s of 6) and the player follows.
        wait(app.buttons["player-play"], "exists == true")
        let slider = app.sliders["player-position"]
        wait(slider, "exists == true")
        slider.adjust(toNormalizedSliderPosition: 0.5)
        wait(app.staticTexts["player-elapsed"], "label == '0:02' OR label == '0:03'")

        // The queue, below the player: rows with their titles; tap one to play it.
        let first = app.buttons["queue-0"]
        let second = app.buttons["queue-1"]
        for _ in 0..<4 where !second.exists { app.swipeUp() }
        wait(second, "label BEGINSWITH 'song'", timeout: 30, in: app)
        second.tap()
        wait(title, "label == 'song'")
        app.buttons["player-pause"].tap()
        wait(app.buttons["player-play"], "exists == true")
        // Play next brings long after song; a swipe removes it again.
        first.press(forDuration: 1.2)
        app.buttons["Play next"].tap()
        wait(second, "label BEGINSWITH 'long'")
        wait(app.buttons["player-next"], "isEnabled == true")
        second.swipeLeft()
        app.buttons["Delete"].tap()
        wait(second, "exists == false")
        // Removing the last one empties the queue: the player closes itself.
        first.press(forDuration: 1.2)
        app.buttons["Remove"].tap()
        wait(title, "exists == false")
        wait(app.staticTexts["now-title"], "exists == false")

        // From a playlist: add to an empty queue plays; play next comes after it.
        app.buttons["song-1"].press(forDuration: 1.2)
        app.buttons["Add to queue"].tap()
        wait(app.staticTexts["now-title"], "label == 'song'")
        // Paused, so the song is still there when the menu is done (the simulator is slow).
        app.buttons["bar-pause"].tap()
        wait(app.buttons["bar-play"], "exists == true")
        app.buttons["song-0"].press(forDuration: 1.2)
        app.buttons["Play next"].tap()
        wait(app.buttons["bar-next"], "isEnabled == true", in: app)
        app.buttons["bar-next"].tap()
        wait(app.staticTexts["now-title"], "label == 'long'", timeout: 15)
        app.buttons["bar-stop"].tap()
    }

    func testAVideoShowsInThePlayerAndPlaysOnWithoutIt() {
        let app = XCUIApplication()
        app.launchArguments = ["-selftest", "music", "-server", server]
        app.launch()
        app.tabBars.buttons["Videos"].tap()
        let playlist = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Video Playlist'")).firstMatch
        wait(playlist, "exists == true", timeout: 180, in: app)
        XCTAssertTrue(playlist.label.contains("2 videos"), playlist.label)
        playlist.tap()
        wait(app.buttons["song-1"], "exists == true", timeout: 30)
        app.buttons["play-all"].tap()
        wait(app.staticTexts["now-title"], "label == 'long'")

        // The player shows the picture; closing it, the picture goes and the sound plays on.
        let bar = app.otherElements["open-player"]
        let picture = app.descendants(matching: .any)["video-state"]
        bar.tap()
        wait(picture, "value == 'shown'", timeout: 20, in: app)
        app.buttons["close-player"].tap()
        wait(picture, "exists == false")

        // Full screen and back.
        bar.tap()
        wait(picture, "value == 'shown'", timeout: 20, in: app)
        app.buttons["full-screen"].tap()
        let full = app.otherElements["full-video"]
        wait(full, "exists == true", in: app)
        wait(app.descendants(matching: .any)["full-video-state"], "value == 'shown'", timeout: 20, in: app)
        full.tap()
        app.buttons["leave-full-screen"].tap()
        wait(full, "exists == false", in: app)
        wait(picture, "value == 'shown'", timeout: 20, in: app)

        // To the home screen and back: the picture comes back.
        XCUIDevice.shared.press(.home)
        sleep(2)
        app.activate()
        wait(picture, "value == 'shown'", timeout: 20, in: app)
        app.buttons["close-player"].tap()
        app.buttons["bar-stop"].tap()
    }

    func testOwnPlaylistsBrowsingAndSearch() {
        let app = XCUIApplication()
        app.launchArguments = ["-selftest", "music", "-server", server]
        app.launch()
        app.tabBars.buttons["Music"].tap()
        let saved = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Music Playlist'")).firstMatch
        wait(saved, "exists == true", timeout: 180, in: app)
        // Names no earlier run on this phone used.
        let suffix = UInt32.random(in: 1000...9999)
        let mine = "Mine \(suffix)", ours = "Ours \(suffix)", copy = "Ours \(suffix) (copy)"
        /// The playlist row named `name` (not one whose name only starts with it).
        func row(_ name: String) -> XCUIElement {
            let longer = [mine, ours, copy].filter { $0 != name && $0.hasPrefix(name) }
            let format = (["label BEGINSWITH %@"] + longer.map { _ in "NOT (label BEGINSWITH %@)" }).joined(separator: " AND ")
            return app.buttons.matching(NSPredicate(format: format, argumentArray: [name] + longer)).firstMatch
        }

        // A new playlist.
        app.buttons["new-playlist"].tap()
        let field = app.alerts.textFields.firstMatch
        wait(field, "exists == true", in: app)
        field.tap()
        field.typeText(mine)
        app.alerts.buttons["OK"].tap()
        wait(row(mine), "exists == true", in: app)

        // Songs, A to Z; both into it, song first.
        app.segmentedControls.buttons["Songs"].tap()
        wait(app.buttons["item-1"], "exists == true", timeout: 30, in: app)
        let titles = (0..<6).map { app.buttons["item-\($0)"] }.filter(\.exists).map(\.label)
        XCTAssertEqual(titles, titles.sorted(), "\(titles)")
        for title in ["song", "long"] {
            guard let at = titles.firstIndex(where: { $0.hasPrefix(title) }) else {
                return XCTFail("no \(title) in \(titles)")
            }
            app.buttons["item-\(at)"].press(forDuration: 1.2)
            app.buttons["Add to playlist…"].tap()
            let pick = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'pick-' AND label BEGINSWITH %@", mine)).firstMatch
            wait(pick, "exists == true", in: app)
            pick.tap()
            wait(pick, "exists == false", in: app)
        }
        app.segmentedControls.buttons["Newest"].tap()
        wait(app.buttons["item-1"], "exists == true", in: app)

        // In it: move up, take out.
        app.segmentedControls.buttons["Playlists"].tap()
        wait(row(mine), "label CONTAINS '2 songs'", in: app)
        row(mine).tap()
        let count = app.staticTexts["entry-count"]
        wait(count, "label == '2 songs'", in: app)
        wait(app.buttons["song-1"], "label BEGINSWITH 'long'", in: app)
        XCTAssertTrue(app.buttons["song-0"].label.hasPrefix("song"), app.buttons["song-0"].label)
        app.buttons["song-1"].press(forDuration: 1.2)
        app.buttons["Move up"].tap()
        wait(app.buttons["song-0"], "label BEGINSWITH 'long'", in: app)
        app.buttons["song-0"].press(forDuration: 1.2)
        app.buttons["Remove from playlist"].tap()
        wait(count, "label == '1 song'", in: app)
        wait(app.buttons["song-0"], "label BEGINSWITH 'song'", in: app)
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // Rename, copy, delete the copy.
        wait(row(mine), "exists == true", in: app)
        row(mine).press(forDuration: 1.2)
        app.buttons["Rename"].tap()
        wait(field, "exists == true", in: app)
        field.tap()
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: mine.count + 4) + ours)
        app.alerts.buttons["OK"].tap()
        wait(row(ours), "exists == true", in: app)
        row(ours).press(forDuration: 1.2)
        app.buttons["Copy"].tap()
        wait(field, "exists == true", in: app)
        app.alerts.buttons["OK"].tap()
        wait(row(copy), "exists == true", in: app)
        row(copy).press(forDuration: 1.2)
        app.buttons["Delete"].tap()
        wait(app.alerts.buttons["Delete"], "exists == true", in: app)
        app.alerts.buttons["Delete"].tap()
        wait(row(copy), "exists == false", in: app)

        // A saved playlist is not changed by hand.
        saved.tap()
        wait(app.staticTexts["saved-note"], "exists == true", in: app)
        app.buttons["song-0"].press(forDuration: 1.2)
        wait(app.buttons["Play next"], "exists == true", in: app)
        XCTAssertFalse(app.buttons["Remove from playlist"].exists)
        XCTAssertFalse(app.buttons["Move down"].exists)
        // Closing the menu by picking from it: the playlist picker, then Cancel.
        app.buttons["Add to playlist…"].tap()
        wait(app.buttons["pick-new"], "exists == true", in: app)
        app.buttons["Cancel"].tap()
        wait(app.buttons["pick-new"], "exists == false", in: app)
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // Search: songs (any case) and playlists; a song found plays.
        let search = app.searchFields.firstMatch
        wait(search, "exists == true", in: app)
        search.tap()
        search.typeText("SONG")
        wait(app.buttons["found-0"], "label BEGINSWITH 'song'", in: app)
        app.buttons["found-0"].tap()
        wait(app.staticTexts["now-title"], "label == 'song'", in: app)
        app.buttons["bar-stop"].tap()
        wait(app.buttons["bar-stop"], "exists == false", in: app)
        search.tap()
        search.buttons["Clear text"].tap()
        search.typeText(ours.lowercased())
        wait(app.buttons["found-playlist-0"], "label BEGINSWITH %@", ours, in: app)
        app.buttons["found-playlist-0"].tap()
        wait(count, "label == '1 song'", in: app)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        search.tap()
        search.buttons["Clear text"].tap()
        search.typeText("nothing like this")
        wait(app.staticTexts["Nothing found."], "exists == true", in: app)
        app.buttons["Cancel"].tap()

        // The test songs name no artist or album.
        app.segmentedControls.buttons["Artists"].tap()
        wait(app.staticTexts["No artists yet."], "exists == true", in: app)
        app.segmentedControls.buttons["Albums"].tap()
        wait(app.staticTexts["No albums yet."], "exists == true", in: app)

        // Clean up for the tests after this one.
        app.segmentedControls.buttons["Playlists"].tap()
        wait(row(ours), "exists == true", in: app)
        row(ours).press(forDuration: 1.2)
        app.buttons["Delete"].tap()
        app.alerts.buttons["Delete"].tap()
        wait(row(ours), "exists == false", in: app)
    }

    /// Waits for `predicate` to hold for `element`; if it never does, prints what `app`
    /// shows (its accessibility tree) to say why.
    private func wait(_ element: XCUIElement, _ predicate: String, _ arguments: CVarArg..., timeout: TimeInterval = 15,
                      in app: XCUIApplication? = nil, file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: predicate, argumentArray: arguments.map { $0 as Any }), object: element)
        let result = XCTWaiter().wait(for: [expectation], timeout: timeout)
        if result != .completed {
            print((app ?? XCUIApplication()).debugDescription)
        }
        XCTAssertEqual(result, .completed, "\(element) never \(predicate)", file: file, line: line)
    }
}
