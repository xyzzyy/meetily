import XCTest

final class LaunchTests: XCTestCase {
    func testDownloadedAppLoadsItsSetupPage() throws {
        continueAfterFailure = false
        let path = try XCTUnwrap(ProcessInfo.processInfo.environment["MEETILY_APP_PATH"])
        let app = XCUIApplication(url: URL(fileURLWithPath: path))
        addTeardownBlock {
            let tree = XCTAttachment(string: app.debugDescription)
            tree.lifetime = .keepAlways
            self.add(tree)
            app.terminate()
        }

        // Use the unchanged release app. No injected code or audio permissions.
        app.launch()
        // A background window does not update its web page or take the first click.
        app.activate()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 60))
        let start = app.buttons["Get Started"]
        XCTAssertTrue(start.waitForExistence(timeout: 60), "The setup page must load in WKWebView.")
        app.activate()
        start.click()
        let setup = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "Setup Overview")).firstMatch
        // If the first click only focused the window, the welcome page is still there.
        if !setup.waitForExistence(timeout: 5), start.exists {
            start.click()
        }
        XCTAssertTrue(setup.waitForExistence(timeout: 30), "The React page must respond to input.")
        XCTAssertTrue(app.buttons["Let's Go"].exists)
        // Stop before model downloads and microphone/screen permission requests.
    }
}
