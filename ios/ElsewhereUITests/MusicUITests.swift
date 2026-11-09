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

    /// Waits for `predicate` to hold for `element`.
    private func wait(_ element: XCUIElement, _ predicate: String, timeout: TimeInterval = 15,
                      file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: predicate), object: element)
        let result = XCTWaiter().wait(for: [expectation], timeout: timeout)
        XCTAssertEqual(result, .completed, "\(element) never \(predicate)", file: file, line: line)
    }
}
