// Stand-in for the Intents slice communication notifications use.

import Foundation
import UserNotifications

public struct INPersonHandleType: Equatable, Sendable {
    let raw: Int
    public static let unknown = INPersonHandleType(raw: 0)
}

public struct INPersonSuggestionType: Equatable, Sendable {
    let raw: Int
    public static let none = INPersonSuggestionType(raw: 0)
}

public struct INOutgoingMessageType: Equatable, Sendable {
    let raw: Int
    public static let outgoingMessageText = INOutgoingMessageType(raw: 1)
}

public struct INInteractionDirection: Equatable, Sendable {
    let raw: Int
    public static let incoming = INInteractionDirection(raw: 2)
}

open class INPersonHandle: NSObject {
    public init(value: String?, type: INPersonHandleType) {}
}

open class INImage: NSObject {
    public init(imageData: Data) {}
    public init?(url: URL) {}
    public init(named name: String) {}
}

open class INPerson: NSObject {
    public init(personHandle: INPersonHandle, nameComponents: PersonNameComponents?, displayName: String?,
                image: INImage?, contactIdentifier: String?, customIdentifier: String?, isMe: Bool,
                suggestionType: INPersonSuggestionType) {}
}

open class INSpeakableString: NSObject {
    public init(spokenPhrase: String) {}
}

open class INIntent: NSObject {}
open class INIntentResponse: NSObject {}
open class INSendMessageAttachment: NSObject {}

open class INSendMessageIntent: INIntent, UNNotificationContentProviding {
    public init(recipients: [INPerson]?, outgoingMessageType: INOutgoingMessageType, content: String?,
                speakableGroupName: INSpeakableString?, conversationIdentifier: String?, serviceName: String?,
                sender: INPerson?, attachments: [INSendMessageAttachment]?) {}
}

open class INInteraction: NSObject {
    public init(intent: INIntent, response: INIntentResponse?) {}
    open var direction: INInteractionDirection = .incoming
    open func donate() async throws {}
}
