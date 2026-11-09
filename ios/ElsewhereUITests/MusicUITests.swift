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
        app.buttons["speed"].tap()
        app.buttons["1x"].tap()
        wait(app.buttons["speed"], "label CONTAINS '1x'")

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
        app.buttons["song-0"].press(forDuration: 1.2)
        app.buttons["Play next"].tap()
        wait(app.buttons["bar-next"], "isEnabled == true")
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
        wait(app.buttons["bar-pause"], "exists == true")

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

    /// Waits for `predicate` to hold for `element`; if it never does, prints what `app`
    /// shows (its accessibility tree) to say why.
    private func wait(_ element: XCUIElement, _ predicate: String, timeout: TimeInterval = 15,
                      in app: XCUIApplication? = nil, file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: predicate), object: element)
        let result = XCTWaiter().wait(for: [expectation], timeout: timeout)
        if result != .completed {
            print((app ?? XCUIApplication()).debugDescription)
        }
        XCTAssertEqual(result, .completed, "\(element) never \(predicate)", file: file, line: line)
    }
}
