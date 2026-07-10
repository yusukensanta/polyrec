# Signs one artifact via SignPath's plain REST API, waits for completion,
# and downloads the signed result.
#
# NOT using SignPath's GitHub Actions connector/action here (signpath/
# github-action-submit-signing-request) -- that connector requires a
# "Trusted Build System" linked at the SignPath *organization* level for
# origin verification, which is not available on the free/Foundation OSS
# tier this project's certificate comes from. The generic REST API this
# script calls instead has no such requirement: bearer-token auth tied to a
# submitter user on the signing policy is sufficient. See:
# https://docs.signpath.io/build-system-integration
#
# Every signing request on this project's policy requires manual approval
# (release-signing policy, see installer/polyrec.iss's comment) -- this
# script polls for up to 30 minutes to give a human time to approve it in
# the SignPath dashboard while the workflow waits.
function Submit-SignPathRequest {
    param(
        [Parameter(Mandatory)] [string]$ArtifactPath,
        [Parameter(Mandatory)] [string]$ArtifactConfigurationSlug,
        [Parameter(Mandatory)] [string]$Description,
        [Parameter(Mandatory)] [string]$OutputPath
    )

    $organizationId = "decb64a5-8938-4442-8848-36981e8328f0"
    $projectSlug = "polyrec"
    $signingPolicySlug = "polyrec"
    $apiBase = "https://app.signpath.io/API/v1/$organizationId"
    $headers = @{ Authorization = "Bearer $env:SIGNPATH_API_TOKEN" }

    $submitResponse = Invoke-WebRequest -Uri "$apiBase/SigningRequests/SubmitWithArtifact" `
        -Method Post `
        -Headers $headers `
        -Form @{
            projectSlug               = $projectSlug
            signingPolicySlug         = $signingPolicySlug
            artifactConfigurationSlug = $ArtifactConfigurationSlug
            artifact                  = Get-Item $ArtifactPath
            description               = $Description
        }
    $requestUrl = $submitResponse.Headers.Location[0]
    Write-Host "Signing request submitted: $requestUrl"

    $timeoutSeconds = 1800
    $intervalSeconds = 15
    $elapsed = 0
    do {
        Start-Sleep -Seconds $intervalSeconds
        $elapsed += $intervalSeconds
        $status = Invoke-RestMethod -Uri $requestUrl -Headers $headers
        Write-Host "Signing request status: $($status.status) (waited ${elapsed}s)"
        if ($elapsed -ge $timeoutSeconds) {
            Write-Error "Timed out waiting for the signing request to complete -- did someone approve it in the SignPath dashboard?"
            exit 1
        }
    } while ($status.status -notin @("Completed", "Failed", "Denied", "Canceled"))

    if ($status.status -ne "Completed") {
        Write-Error "Signing request ended with status: $($status.status)"
        exit 1
    }

    Invoke-WebRequest -Uri "$requestUrl/SignedArtifact" -Headers $headers -OutFile $OutputPath
    Write-Host "Signed artifact saved to $OutputPath"
}
