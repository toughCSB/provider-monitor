import XCTest
@testable import ProviderMonitor

final class ChatGPTSavedAccountsTests: XCTestCase {
    private func session(user: String, account: String) throws -> Data {
        let claims = try JSONSerialization.data(withJSONObject: [
            "https://api.openai.com/auth": ["chatgpt_account_id": account]
        ]).base64EncodedString()
        return try JSONSerialization.data(withJSONObject: [
            "user": ["id": user], "accessToken": "header.\(claims).signature"
        ])
    }

    func testEachBrowserAccountUsesItsOwnSessionAndWorkspaceIdentity() throws {
        let first = try ChatGPTSavedAccount.decodeSession(session(user: "user-a", account: "workspace-a"), expectedUserID: "user-a")
        let second = try ChatGPTSavedAccount.decodeSession(session(user: "user-b", account: "workspace-b"), expectedUserID: "user-b")
        XCTAssertEqual(first.accountID, "workspace-a")
        XCTAssertEqual(second.accountID, "workspace-b")
        XCTAssertNotEqual(first.accessToken, second.accessToken)
        XCTAssertThrowsError(try ChatGPTSavedAccount.decodeSession(session(user: "user-b", account: "workspace-b"), expectedUserID: "user-a")) { error in
            guard case UsageProviderError.accountChanged = error else { return XCTFail("must reject a different signed-in user") }
        }
    }

    func testSignedOutOrMalformedSessionsNeverFallBackToTheCLIAccount() throws {
        for value in ["{}", "{\"user\":{\"id\":\"a\"}}", "{\"accessToken\":\"bad\",\"user\":{\"id\":\"a\"}}"] {
            XCTAssertThrowsError(try ChatGPTSavedAccount.decodeSession(Data(value.utf8), expectedUserID: "a"))
        }
    }

    func testSavedAccountListRejectsDuplicatesPlaceholdersAndCookieInjection() throws {
        let data = try JSONSerialization.data(withJSONObject: [
            ["userId": "a", "sessionToken": "safe-a"],
            ["userId": "a", "sessionToken": "copy-a"],
            ["userId": "placeholder", "sessionToken": "__current_account_session__"],
            ["userId": "bad", "sessionToken": "value; other=cookie"],
            ["userId": "b", "sessionToken": "safe-b"],
            ["userId": "c", "sessionToken": "safe-c"]
        ])
        XCTAssertEqual(try ChatGPTSavedAccounts.decodeEntries(data).map(\.userId), ["a", "b"])
    }

    func testProviderIdentityDoesNotDependOnEmailOrCurrentAccountOrder() {
        let directory = URL(fileURLWithPath: "/test/leveldb")
        let first = ChatGPTSavedAccount(directory: directory, userID: "user-a", label: "old@test")
        let renamed = ChatGPTSavedAccount(directory: directory, userID: "user-a", label: "new@test")
        let second = ChatGPTSavedAccount(directory: directory, userID: "user-b", label: "other@test")
        XCTAssertEqual(first.providerID, renamed.providerID)
        XCTAssertNotEqual(first.providerID, second.providerID)
        XCTAssertFalse(first.providerID.contains("user-a"))
        XCTAssertTrue(CodexProfile.isCodex(providerID: first.providerID))
    }
}
