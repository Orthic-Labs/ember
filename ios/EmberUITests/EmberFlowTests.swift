import XCTest

/// End-to-end: the real app over the real Rust engine, on the procedural sample library.
/// Screenshots are kept in the result bundle (`xcrun xcresulttool export attachments`).
final class EmberFlowTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    @MainActor
    func testBrowseCullEditAndUndo() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-EmberResetLibrary"]
        app.launch()
        snap(app, "1-chooser")

        app.buttons["Open Sample Library"].tap()
        let firstPhoto = app.scrollViews.buttons.firstMatch
        XCTAssertTrue(firstPhoto.waitForExistence(timeout: 30), "sample library shows a grid")
        sleep(3) // let thumbnails render for the screenshot
        snap(app, "2-grid")

        firstPhoto.tap()
        XCTAssertTrue(app.buttons["Edit"].waitForExistence(timeout: 10))
        app.buttons["4 stars"].tap()
        app.buttons["Pick"].tap()
        XCTAssertTrue(app.buttons["Remove pick"].waitForExistence(timeout: 10), "pick round-trips through the catalog")
        sleep(2)
        snap(app, "3-viewer-rated")

        app.buttons["Edit"].tap()
        let exposure = app.sliders["Exposure"]
        XCTAssertTrue(exposure.waitForExistence(timeout: 10), "Light sliders come from the Rust control specs")
        for _ in 0..<4 where !exposure.isHittable { app.tables.firstMatch.swipeUp() }
        XCTAssertTrue(exposure.isHittable, "Exposure slider is on screen")
        exposure.adjust(toNormalizedSliderPosition: 0.65)
        sleep(4)
        snap(app, "4-edited")

        let undo = app.buttons["Undo"]
        let redo = app.buttons["Redo"]
        XCTAssertTrue(undo.isEnabled, "a slider gesture becomes an undo step")
        undo.tap()
        XCTAssertTrue(redo.waitForEnabled(timeout: 10), "undo of the slider gesture enables redo")
        sleep(2)
        snap(app, "5-undone")
        redo.tap()
        sleep(2)
        app.buttons["Done"].tap()

        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.scrollViews.buttons.firstMatch.waitForExistence(timeout: 10))
        let editedCell = app.scrollViews.buttons.matching(NSPredicate(format: "value == 'edited'")).firstMatch
        XCTAssertTrue(editedCell.waitForExistence(timeout: 10), "the edit is in the catalog: the grid marks the photo edited")
    }

    @MainActor
    private func snap(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

private extension XCUIElement {
    func waitForEnabled(timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isEnabled == true"), object: self)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }
}
