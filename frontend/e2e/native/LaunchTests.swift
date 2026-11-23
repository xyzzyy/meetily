import XCTest

final class LaunchTests: XCTestCase {
    private func waitUntilHittable(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "isHittable == true"),
            object: element
        )
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }

    func testDownloadedAppLoadsItsSetupPage() throws {
        continueAfterFailure = false
        let path = try XCTUnwrap(ProcessInfo.processInfo.environment["MEETILY_APP_PATH"])
        let app = XCUIApplication(url: URL(fileURLWithPath: path))
        addTeardownBlock {
            if (self.testRun?.totalFailureCount ?? 0) > 0 {
                let tree = XCTAttachment(string: app.debugDescription)
                tree.lifetime = .keepAlways
                self.add(tree)
            }
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
        XCTAssertTrue(waitUntilHittable(start, timeout: 10), "The Get Started button must become hittable.")
        start.click()
        let setup = app.buttons["Let's Go"]
        // If the first click only focused the window, the welcome page is still there.
        if !setup.waitForExistence(timeout: 5), start.exists {
            app.activate()
            if waitUntilHittable(start, timeout: 5) {
                start.click()
            }
        }
        XCTAssertTrue(setup.waitForExistence(timeout: 30), "The React page must respond to input.")
        // Stop before model downloads and microphone/screen permission requests.
    }
}
