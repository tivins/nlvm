namespace tools;

class Release {
    private static int run() throws IOException {
        string changelog = system.io.File.readAllText("CHANGELOG.md");
        auto header = system.text.Regex.matchFirst("## \\[([^\\]]+)\\]", changelog);
        if (header == null) {
            system.Err.println("No version header found in CHANGELOG.md (expected \"## [x.y.z]\").");
            return 1;
        }
        string changelogVersion = header.groups[1];
        string specsVersion = system.io.File.readAllText("SPECS_VERSION").trim();
        string version = changelogVersion + "+" + specsVersion;
        system.Out.println("Latest version: " + version);

        string tagMessage = "Release " + changelogVersion + "\n" + "Specs " + specsVersion;
        auto tagResult = system.ps.Process.run(new string[]{"git", "tag", "-a", version, "-m", tagMessage});
        if (tagResult.exitCode != 0) {
            system.Err.print(tagResult.stderr);
            return tagResult.exitCode;
        }
        system.Out.println("Created tag " + version + ".");

        auto pushResult = system.ps.Process.run(new string[]{"git", "push", "origin", version});
        if (pushResult.exitCode != 0) {
            system.Err.print(pushResult.stderr);
            return pushResult.exitCode;
        }
        system.Out.println("Pushed tag " + version + " to origin.");

        return 0;
    }

    public static int main(string[] args) {
        try {
            return Release.run();
        } catch (IOException ex) {
            system.Err.println("Error: " + ex.message);
            return 1;
        }
    }
}
