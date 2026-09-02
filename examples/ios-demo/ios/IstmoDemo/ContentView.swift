import SwiftUI

struct ContentView: View {

    @State private var input: String = "hola"
    @State private var output: String = ""
    @State private var inFlight: Bool = false

    private let echo = EchoClient()

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("istmo iOS demo")
                .font(.title2)
                .fontWeight(.semibold)

            Text("Text goes to Rust, comes back prefixed. Every call round-trips through the frame protocol.")
                .font(.footnote)
                .foregroundColor(.secondary)

            TextField("Message", text: $input)
                .textFieldStyle(.roundedBorder)
                .disableAutocorrection(true)

            Button(action: run) {
                HStack {
                    if inFlight {
                        ProgressView().padding(.trailing, 4)
                    }
                    Text(inFlight ? "Calling Rust…" : "Echo via Rust")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent)
            .disabled(inFlight)

            Text("Result")
                .font(.headline)
            ScrollView {
                Text(output.isEmpty ? "(no response yet)" : output)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .background(Color(.secondarySystemBackground))
                    .cornerRadius(8)
            }
            .frame(maxHeight: 200)

            Spacer()
        }
        .padding()
    }

    private func run() {
        inFlight = true
        let message = input
        Task {
            defer { inFlight = false }
            do {
                let reply = try await echo.echo(message)
                await MainActor.run { output = reply }
            } catch let e as EchoException {
                await MainActor.run { output = "domain error: \(e.reason)" }
            } catch {
                await MainActor.run { output = "runtime error: \(error)" }
            }
        }
    }
}
